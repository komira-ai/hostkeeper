//! The web app's modules in a JavaScript engine (boa, pure Rust), against a fake gateway.
//!
//! [`Ui::start`] parses every module of `ui/` from the text this crate was built with, under
//! `ui/<path>`, and `tests/support/kit.js` under `test/kit.js`. [`Ui::run`] evaluates a driver
//! module (at `ui/driver.js`, so it imports `./api.js`, `./pages/fleet.js` and `../test/kit.js`)
//! and calls its exported `async function main()`, running the engine's jobs until the promise
//! settles; the value it resolves to comes back as JSON.
//!
//! The fake gateway is the global function `gatewayFetch(url, init)`, written in Rust. It
//! records each request and answers with the next reply scripted for the request's path, or, when
//! none is left, `501 {"code":"UNIMPLEMENTED","reason":"unimplemented"}`. A reply may be one whose
//! body cannot be read, or one held back until a driver calls `gatewayRelease(method)` (also
//! Rust), so a test decides the order in which replies arrive. A scripted success is checked
//! against the method's response message in the descriptors, and every recorded request body
//! against its request message, by [`canonical`]: the canonical JSON of each, with no unknown
//! field, no field-name alias and 64-bit integers as strings.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of the support module"
)]

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::rc::Rc;

use boa_engine::builtins::promise::PromiseState;
use boa_engine::gc::{Gc, GcRefCell};
use boa_engine::module::MapModuleLoader;
use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::object::{JsObject, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsArgs, JsNativeError, JsResult, JsString, JsValue, Module, NativeFunction, Source,
    js_string,
};
use hostkeeper_ui_tests::UI_FILES;
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor,
};
use serde_json::{Value, json};

pub mod mdm_setup;
pub mod pages;

/// The fake document, tree queries and page context the drivers share.
const KIT: &str = include_str!("kit.js");

/// The reply to a call no reply was scripted for: what the gateway answers for a method whose
/// handler has not merged.
pub const UNIMPLEMENTED: &str = r#"{"code":"UNIMPLEMENTED","reason":"unimplemented"}"#;

/// The sign-in routes, as the fake gateway names a request (its URL without the leading `/`).
pub const LOGIN: &str = "v1/login";
pub const LOGOUT: &str = "v1/logout";
pub const SESSION: &str = "v1/session";
pub const PASSWORD: &str = "v1/password";

/// How a scripted reply arrives.
const PLAIN: u8 = 0;
/// Its status arrives, and reading its body fails.
const UNREADABLE: u8 = 1;
/// It arrives when a driver calls `gatewayRelease(method)`.
const HELD: u8 = 2;

/// A reply's HTTP status (0 for a request that gets no reply: `fetch` rejects), body and how it
/// arrives.
type Reply = (u16, String, u8);
type Requests = Gc<GcRefCell<Vec<String>>>;
type Replies = Gc<GcRefCell<BTreeMap<String, VecDeque<Reply>>>>;
/// The held replies: the method, the response and the function that resolves `fetch`'s promise.
type Held = Gc<GcRefCell<Vec<(String, JsObject, JsFunction)>>>;

/// One request the app sent: the URL it fetched and the `init` it passed, as JSON.
#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
    pub init: Value,
}

impl Request {
    /// The method the request calls: its URL without the leading `/`.
    pub fn method(&self) -> &str {
        self.url
            .strip_prefix('/')
            .expect("a request path starts with /")
    }

    /// The HTTP method.
    pub fn http_method(&self) -> &str {
        self.init["method"]
            .as_str()
            .expect("a request names its HTTP method")
    }

    /// A header's value, by its name in any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        let headers = self.init["headers"].as_object()?;
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| v.as_str())
    }

    /// The body, parsed.
    pub fn body(&self) -> Value {
        let text = self.init["body"]
            .as_str()
            .expect("a request has a text body");
        serde_json::from_str(text).expect("the body is JSON")
    }
}

/// The modules of the app in one engine, with the fake gateway.
pub struct Ui {
    context: Context,
    requests: Requests,
    replies: Replies,
    held: Held,
    pool: DescriptorPool,
}

fn fake_fetch(
    _this: &JsValue,
    args: &[JsValue],
    gateway: &(Requests, Replies, Held),
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = args
        .get_or_undefined(0)
        .to_string(context)?
        .to_std_string_escaped();
    let init = args
        .get_or_undefined(1)
        .to_json(context)?
        .unwrap_or(Value::Null);
    gateway
        .0
        .borrow_mut()
        .push(json!({"url": url, "init": init}).to_string());
    let method = url.trim_start_matches('/').to_owned();
    let scripted = gateway
        .1
        .borrow_mut()
        .get_mut(&method)
        .and_then(VecDeque::pop_front);
    let (status, body, how) = scripted.unwrap_or((501, UNIMPLEMENTED.to_owned(), PLAIN));
    if status == 0 {
        let error = JsNativeError::typ().with_message("the network is unreachable");
        return Ok(JsPromise::reject(error, context)?.into());
    }
    let text = if how == UNREADABLE {
        NativeFunction::from_copy_closure(|_, _, context| {
            let error = JsNativeError::typ().with_message("the body cannot be read");
            Ok(JsPromise::reject(error, context)?.into())
        })
    } else {
        NativeFunction::from_copy_closure_with_captures(
            |_, _, body: &JsString, context| Ok(JsPromise::resolve(body.clone(), context)?.into()),
            JsString::from(body.as_str()),
        )
    };
    let response = ObjectInitializer::new(context)
        .property(js_string!("status"), i32::from(status), Attribute::all())
        .property(
            js_string!("ok"),
            (200..300).contains(&status),
            Attribute::all(),
        )
        .function(text, js_string!("text"), 0)
        .build();
    if how == HELD {
        let (promise, resolvers) = JsPromise::new_pending(context);
        gateway
            .2
            .borrow_mut()
            .push((method, response, resolvers.resolve));
        return Ok(promise.into());
    }
    Ok(JsPromise::resolve(response, context)?.into())
}

/// `gatewayRelease(method)`: delivers the oldest held reply of `method`; false when none is held.
fn release(
    _this: &JsValue,
    args: &[JsValue],
    held: &Held,
    context: &mut Context,
) -> JsResult<JsValue> {
    let method = args
        .get_or_undefined(0)
        .to_string(context)?
        .to_std_string_escaped();
    let found = {
        let mut held = held.borrow_mut();
        let at = held.iter().position(|(m, _, _)| *m == method);
        at.map(|at| held.remove(at))
    };
    let Some((_, response, resolve)) = found else {
        return Ok(false.into());
    };
    resolve.call(&JsValue::undefined(), &[response.into()], context)?;
    Ok(true.into())
}

fn is_digits(text: &str, signed: bool) -> bool {
    let digits = if signed {
        text.strip_prefix('-').unwrap_or(text)
    } else {
        text
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `value` is the canonical JSON of one value of `kind`.
fn canonical_value(kind: &Kind, value: &Value, at: &str) -> Result<(), String> {
    let fits = match kind {
        Kind::Message(message) => return canonical(message, value, at),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            value.as_str().is_some_and(|t| is_digits(t, true))
        }
        Kind::Uint64 | Kind::Fixed64 => value.as_str().is_some_and(|t| is_digits(t, false)),
        Kind::Enum(_) | Kind::String | Kind::Bytes => value.is_string(),
        Kind::Bool => value.is_boolean(),
        _ => value.is_number(),
    };
    if fits {
        Ok(())
    } else {
        Err(format!(
            "{at}: {value} is not the canonical JSON of a {kind:?}"
        ))
    }
}

fn canonical_field(field: &FieldDescriptor, value: &Value, at: &str) -> Result<(), String> {
    let kind = field.kind();
    if field.is_map() {
        let entry = kind
            .as_message()
            .expect("a map field is a message of entries");
        let values = entry.map_entry_value_field().kind();
        let map = value
            .as_object()
            .ok_or_else(|| format!("{at}: a map is an object"))?;
        for (key, item) in map {
            canonical_value(&values, item, &format!("{at}.{key}"))?;
        }
        Ok(())
    } else if field.is_list() {
        let list = value
            .as_array()
            .ok_or_else(|| format!("{at}: a repeated field is an array"))?;
        for (i, item) in list.iter().enumerate() {
            canonical_value(&kind, item, &format!("{at}[{i}]"))?;
        }
        Ok(())
    } else {
        canonical_value(&kind, value, at)
    }
}

/// Whether `value` is the canonical proto3 JSON of a `message`: each key the lowerCamelCase JSON
/// name of a field (not the field's proto name, when the two differ), each 64-bit integer a
/// string of digits, each enum by name, each other number a number, and nothing else; and that
/// it reads as one with unknown fields refused.
pub fn canonical(message: &MessageDescriptor, value: &Value, at: &str) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{at}: a message is an object, not {value}"))?;
    for (key, item) in object {
        let field = message.get_field_by_json_name(key).ok_or_else(|| {
            format!(
                "{at}: {key} is not the JSON name of a field of {}",
                message.full_name()
            )
        })?;
        canonical_field(&field, item, &format!("{at}.{key}"))?;
    }
    DynamicMessage::deserialize_with_options(
        message.clone(),
        value.clone(),
        &DeserializeOptions::new(),
    )
    .map(|_| ())
    .map_err(|e| format!("{at}: not a {}: {e}", message.full_name()))
}

fn message_of(pool: &DescriptorPool, method: &str, response: bool) -> Option<MessageDescriptor> {
    let (service, name) = method.split_once('/')?;
    let found = pool
        .get_service_by_name(service)?
        .methods()
        .find(|m| m.name() == name)?;
    Some(if response {
        found.output()
    } else {
        found.input()
    })
}

/// Panics unless `value` is the canonical JSON of a `message`.
fn assert_shape(message: MessageDescriptor, value: &Value, what: &str) {
    if let Err(e) = canonical(&message, value, "") {
        panic!("{what} is not canonical JSON: {e}\n{value}");
    }
}

impl Ui {
    /// An engine with every module of the app and the kit, and a gateway with nothing scripted.
    pub fn start() -> Ui {
        let loader = Rc::new(MapModuleLoader::new());
        let mut context = Context::builder()
            .module_loader(loader.clone())
            .build()
            .expect("the engine starts");
        let mut modules: Vec<(String, &str)> = UI_FILES
            .iter()
            .filter(|f| f.is_script())
            .map(|f| (format!("ui/{}", f.path), f.text))
            .collect();
        modules.push(("test/kit.js".to_owned(), KIT));
        for (path, text) in modules {
            let source = Source::from_bytes(text).with_path(Path::new(&path));
            let module = Module::parse(source, None, &mut context)
                .unwrap_or_else(|e| panic!("{path} does not parse: {e}"));
            loader.insert(&path, module);
        }
        let requests: Requests = Gc::new(GcRefCell::new(Vec::new()));
        let replies: Replies = Gc::new(GcRefCell::new(BTreeMap::new()));
        let held: Held = Gc::new(GcRefCell::new(Vec::new()));
        let fetch = NativeFunction::from_copy_closure_with_captures(
            fake_fetch,
            (requests.clone(), replies.clone(), held.clone()),
        );
        context
            .register_global_callable(js_string!("gatewayFetch"), 2, fetch)
            .expect("gatewayFetch is defined");
        let release = NativeFunction::from_copy_closure_with_captures(release, held.clone());
        context
            .register_global_callable(js_string!("gatewayRelease"), 1, release)
            .expect("gatewayRelease is defined");
        let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
            .expect("the descriptors decode");
        Ui {
            context,
            requests,
            replies,
            held,
            pool,
        }
    }

    fn script_as(&self, method: &str, status: u16, body: String, how: u8) {
        self.replies
            .borrow_mut()
            .entry(method.to_owned())
            .or_default()
            .push_back((status, body, how));
    }

    fn script(&self, method: &str, status: u16, body: String) {
        self.script_as(method, status, body, PLAIN);
    }

    /// Whether `value` is the canonical JSON of `method`'s response (`response`) or request.
    pub fn shape(&self, method: &str, response: bool, value: &Value) -> Result<(), String> {
        let message = message_of(&self.pool, method, response)
            .ok_or_else(|| format!("{method} is not a method of the descriptors"))?;
        canonical(&message, value, "")
    }

    /// Scripts a success of `method`: `reply`, which must be its response message's canonical
    /// JSON.
    pub fn reply(&self, method: &str, reply: Value) {
        let message = message_of(&self.pool, method, true)
            .unwrap_or_else(|| panic!("{method} is not a method of the descriptors"));
        assert_shape(message, &reply, &format!("the reply scripted for {method}"));
        self.script(method, 200, reply.to_string());
    }

    /// Scripts a reply of `method` that arrives only when a driver calls
    /// `gatewayRelease(method)`.
    pub fn hold(&self, method: &str, status: u16, body: &str) {
        self.script_as(method, status, body.to_owned(), HELD);
    }

    /// Scripts a reply of `method` with `status` whose body cannot be read.
    pub fn unreadable(&self, method: &str, status: u16) {
        self.script_as(method, status, String::new(), UNREADABLE);
    }

    /// Scripts a refusal of `method` with `status` and the body `{"code", "reason"}`.
    pub fn refuse(&self, method: &str, status: u16, code: &str, reason: &str) {
        self.script(
            method,
            status,
            json!({"code": code, "reason": reason}).to_string(),
        );
    }

    /// Scripts a reply of `method` with any status and body, as text.
    pub fn reply_raw(&self, method: &str, status: u16, body: &str) {
        self.script(method, status, body.to_owned());
    }

    /// Scripts the answer of `GET /v1/session` for a browser that holds a live session of user
    /// `operator` with the CSRF token `csrf`, which the app reads when it starts.
    pub fn session(&self, csrf: &str) {
        self.reply_raw(
            SESSION,
            200,
            &json!({"username": "operator", "csrf": csrf, "must_change": false}).to_string(),
        );
    }

    /// Scripts the answer of `GET /v1/session` for a browser with no session.
    pub fn no_session(&self) {
        self.reply_raw(SESSION, 401, r#"{"error":"sign_in_required"}"#);
    }

    /// Scripts a refusal of one of the four sign-in routes: `{"error": "<token>"}`.
    pub fn refuse_route(&self, route: &str, status: u16, token: &str) {
        self.reply_raw(route, status, &json!({"error": token}).to_string());
    }

    /// Scripts a request of `method` that gets no reply: `fetch` rejects.
    pub fn drop_request(&self, method: &str) {
        self.script(method, 0, String::new());
    }

    /// Every request sent so far, in order. Each body sent to a method of the descriptors is
    /// checked against its request message.
    pub fn requests(&self) -> Vec<Request> {
        let recorded: Vec<Request> = self
            .requests
            .borrow()
            .iter()
            .map(|text| {
                let value: Value = serde_json::from_str(text).expect("a recorded request");
                Request {
                    url: value["url"].as_str().expect("a url").to_owned(),
                    init: value["init"].clone(),
                }
            })
            .collect();
        for request in &recorded {
            if let Some(message) = message_of(&self.pool, request.method(), false) {
                assert_shape(
                    message,
                    &request.body(),
                    &format!("the body sent to {}", request.method()),
                );
            }
        }
        recorded
    }

    /// The methods called so far, in order.
    pub fn methods(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| r.method().to_owned())
            .collect()
    }

    fn settle(&mut self, promise: &JsPromise, what: &str) -> JsValue {
        self.context.run_jobs().expect("the engine's jobs run");
        match promise.state() {
            PromiseState::Fulfilled(value) => value,
            PromiseState::Rejected(error) => {
                let text = error
                    .to_string(&mut self.context)
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_default();
                panic!("{what} failed: {text}")
            }
            PromiseState::Pending => panic!("{what} never settled"),
        }
    }

    /// Evaluates `driver` as the module `ui/driver.js`, calls its `main()` and returns what it
    /// resolves to, as JSON (`null` for `undefined`). A driver that throws fails the test.
    pub fn run(&mut self, driver: &str) -> Value {
        let source = Source::from_bytes(driver).with_path(Path::new("ui/driver.js"));
        let module = Module::parse(source, None, &mut self.context).expect("the driver parses");
        let loaded = module.load_link_evaluate(&mut self.context);
        self.settle(&loaded, "loading the driver");
        let main = module
            .namespace(&mut self.context)
            .get(js_string!("main"), &mut self.context)
            .expect("the driver exports main");
        let main = main.as_callable().expect("main is a function");
        let result = main
            .call(&JsValue::undefined(), &[], &mut self.context)
            .expect("main starts");
        let promise = result.as_promise().expect("main is async");
        let value = self.settle(&promise, "main");
        value
            .to_json(&mut self.context)
            .expect("main resolves to JSON")
            .unwrap_or(Value::Null)
    }
}
