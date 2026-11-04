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
//! none is left, `501 {"code":"UNIMPLEMENTED","reason":"unimplemented"}`. A scripted success is
//! checked against the method's response message in the descriptors, and every recorded request
//! body against its request message (the canonical JSON of each, no unknown field), so the app's
//! field names and the tests' replies are the API's own.

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
use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsArgs, JsNativeError, JsResult, JsString, JsValue, Module, NativeFunction, Source,
    js_string,
};
use hostkeeper_ui_tests::UI_FILES;
use prost_reflect::{DescriptorPool, DeserializeOptions, DynamicMessage, MessageDescriptor};
use serde_json::{Value, json};

pub mod pages;

/// The fake document, tree queries and page context the drivers share.
const KIT: &str = include_str!("kit.js");

/// The reply to a call no reply was scripted for: what the gateway answers for a method whose
/// handler has not merged.
pub const UNIMPLEMENTED: &str = r#"{"code":"UNIMPLEMENTED","reason":"unimplemented"}"#;

/// A reply's HTTP status, 0 for a request that gets no reply (`fetch` rejects), and body.
type Reply = (u16, String);
type Requests = Gc<GcRefCell<Vec<String>>>;
type Replies = Gc<GcRefCell<BTreeMap<String, VecDeque<Reply>>>>;

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
    pool: DescriptorPool,
}

fn fake_fetch(
    _this: &JsValue,
    args: &[JsValue],
    gateway: &(Requests, Replies),
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
    let (status, body) = scripted.unwrap_or((501, UNIMPLEMENTED.to_owned()));
    if status == 0 {
        let error = JsNativeError::typ().with_message("the network is unreachable");
        return Ok(JsPromise::reject(error, context)?.into());
    }
    let text = NativeFunction::from_copy_closure_with_captures(
        |_, _, body: &JsString, context| Ok(JsPromise::resolve(body.clone(), context)?.into()),
        JsString::from(body.as_str()),
    );
    let response = ObjectInitializer::new(context)
        .property(js_string!("status"), i32::from(status), Attribute::all())
        .property(
            js_string!("ok"),
            (200..300).contains(&status),
            Attribute::all(),
        )
        .function(text, js_string!("text"), 0)
        .build();
    Ok(JsPromise::resolve(response, context)?.into())
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

/// Panics unless `value` is the canonical JSON of a `message` (no unknown field).
fn assert_shape(message: MessageDescriptor, value: &Value, what: &str) {
    let full = message.full_name().to_owned();
    if let Err(e) =
        DynamicMessage::deserialize_with_options(message, value.clone(), &DeserializeOptions::new())
    {
        panic!("{what} is not a {full}: {e}\n{value}");
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
        let fetch = NativeFunction::from_copy_closure_with_captures(
            fake_fetch,
            (requests.clone(), replies.clone()),
        );
        context
            .register_global_callable(js_string!("gatewayFetch"), 2, fetch)
            .expect("gatewayFetch is defined");
        let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
            .expect("the descriptors decode");
        Ui {
            context,
            requests,
            replies,
            pool,
        }
    }

    fn script(&self, method: &str, status: u16, body: String) {
        self.replies
            .borrow_mut()
            .entry(method.to_owned())
            .or_default()
            .push_back((status, body));
    }

    /// Scripts a success of `method`: `reply`, which must be its response message's JSON.
    pub fn reply(&self, method: &str, reply: Value) {
        let message = message_of(&self.pool, method, true)
            .unwrap_or_else(|| panic!("{method} is not a method of the descriptors"));
        assert_shape(message, &reply, &format!("the reply scripted for {method}"));
        self.script(method, 200, reply.to_string());
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
