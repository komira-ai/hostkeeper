//! The words of a command line as flags: the global flags, which are taken before the service,
//! between the service and the method, and among a method's flags; and a method's field flags.
//!
//! A flag's value is `--flag=value` or `--flag value`; a value that starts with `--` is given
//! only as `--flag=value`. Everything is on bytes.

use hostkeeper_proto::ctl::{Field, Input, Label};

use crate::encode::{encode, json_text, seconds};
use crate::table::DEFAULT_TIMEOUT_SECS;
use crate::{Flow, Stop};

/// The bytes of `text` up to the first `=`: the flag of `--flag=value`.
pub(crate) fn flag_of(arg: &[u8]) -> &[u8] {
    arg.split(|b| *b == b'=').next().unwrap_or_default()
}

/// The value of the flag that starts `args`, and how many arguments it took: the text after the
/// first `=` of `--flag=value`, or the next argument, which must not start with `--`.
pub(crate) fn value_of(args: &[Vec<u8>]) -> Flow<(&[u8], usize)> {
    let flag = args.first().map(|a| flag_of(a)).unwrap_or_default();
    let first = args.first().map(Vec::as_slice).unwrap_or_default();
    if first.starts_with(b"--")
        && let Some(at) = first.iter().position(|b| *b == b'=')
    {
        return Ok((first.get(at + 1..).unwrap_or_default(), 1));
    }
    match args.get(1) {
        Some(value) if !value.starts_with(b"--") => Ok((value, 2)),
        _ => Err(Stop::usage([flag, b" needs a value"].concat())),
    }
}

/// Refuses a value given to the switch that starts `args`; the switch is one argument.
fn switch(args: &[Vec<u8>]) -> Flow<usize> {
    let first = args.first().map(Vec::as_slice).unwrap_or_default();
    if first.contains(&b'=') {
        return Err(Stop::usage([flag_of(first), b" takes no value"].concat()));
    }
    Ok(1)
}

/// The global flags a command line has given so far.
#[derive(Debug, Default)]
pub(crate) struct Globals {
    pub(crate) endpoint: Option<Vec<u8>>,
    pub(crate) ca_file: Option<Vec<u8>>,
    pub(crate) token_file: Option<Vec<u8>>,
    pub(crate) token_stdin: bool,
    pub(crate) audience: Option<Vec<u8>>,
    pub(crate) response_file: Option<Vec<u8>>,
    timeout: Option<u32>,
    stdin_by: Option<Vec<u8>>,
}

/// Sets `slot` to `value`, or says the flag is given twice.
fn once(slot: &mut Option<Vec<u8>>, flag: &[u8], value: &[u8]) -> Flow<()> {
    if slot.is_some() {
        return Err(Stop::usage([flag, b" is given twice"].concat()));
    }
    *slot = Some(value.to_vec());
    Ok(())
}

impl Globals {
    /// `--timeout-secs`, in seconds.
    pub(crate) fn timeout_secs(&self) -> u32 {
        self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS)
    }

    /// Records that `flag` reads stdin, or says another input does.
    pub(crate) fn claim_stdin(&mut self, flag: &[u8]) -> Flow<()> {
        match &self.stdin_by {
            Some(by) => Err(Stop::usage(
                [
                    by.as_slice(),
                    b" and ",
                    flag,
                    b" both read stdin; at most one input may",
                ]
                .concat(),
            )),
            None => {
                self.stdin_by = Some(flag.to_vec());
                Ok(())
            }
        }
    }

    /// Takes the global flag that starts `args`, with its value; how many arguments it took, or
    /// `None` when `args` do not start with one.
    pub(crate) fn take(&mut self, args: &[Vec<u8>]) -> Flow<Option<usize>> {
        let first = args.first().map(Vec::as_slice).unwrap_or_default();
        let flag = flag_of(first);
        if flag == b"--token-stdin" {
            let took = switch(args)?;
            if self.token_stdin {
                return Err(Stop::usage([flag, b" is given twice"].concat()));
            }
            self.token_stdin = true;
            self.claim_stdin(flag)?;
            return Ok(Some(took));
        }
        let slot = match flag {
            b"--endpoint" => &mut self.endpoint,
            b"--ca-file" => &mut self.ca_file,
            b"--token-file" => &mut self.token_file,
            b"--github-oidc-audience" => &mut self.audience,
            b"--response-file" => &mut self.response_file,
            b"--timeout-secs" => {
                let (value, took) = value_of(args)?;
                if self.timeout.is_some() {
                    return Err(Stop::usage([flag, b" is given twice"].concat()));
                }
                let Some(secs) = seconds(value) else {
                    return Err(Stop::usage(
                        [flag, b" takes a whole number of seconds, 1 to 999999"].concat(),
                    ));
                };
                self.timeout = Some(secs);
                return Ok(Some(took));
            }
            _ => return Ok(None),
        };
        let (value, took) = value_of(args)?;
        once(slot, flag, value)?;
        Ok(Some(took))
    }
}

/// What a method's flags have given for one field.
#[derive(Debug, Clone, Default)]
pub(crate) struct Slot {
    /// Whether the field is set by a flag or a secret.
    pub(crate) set: bool,
    /// The JSON of the flag's values, joined by `,` for a repeated field or a map.
    pub(crate) vals: Vec<u8>,
    /// Where a secret is read from: a path, or `-` for stdin.
    pub(crate) source: Vec<u8>,
    /// The JSON keys of a map, to refuse one twice.
    keys: Vec<Vec<u8>>,
}

/// What a method's flags have given: for each field, whether it is set and the JSON of its
/// value, or where a secret is read from.
pub(crate) struct Given<'t> {
    pub(crate) fields: &'t [Field],
    /// One slot for each of `fields`.
    pub(crate) slots: Vec<Slot>,
}

impl<'t> Given<'t> {
    /// Nothing given for `fields`.
    pub(crate) fn new(fields: &'t [Field]) -> Self {
        Given {
            fields,
            slots: vec![Slot::default(); fields.len()],
        }
    }

    /// One occurrence of the flag of `field`, the `at`th, with `text` as its value.
    fn put(&mut self, at: usize, field: &Field, text: &[u8]) -> Flow<()> {
        let slot = &mut self.slots[at];
        match field.label {
            Label::Repeated => {
                let json = encode(field, text)?;
                slot.append(&json);
            }
            Label::Map => {
                let Some(split) = text.iter().position(|b| *b == b'=') else {
                    return Err(Stop::usage(format!("--{} takes key=value", field.flag)));
                };
                let key = json_text(text.get(..split).unwrap_or_default());
                if slot.keys.contains(&key) {
                    return Err(Stop::usage(
                        [
                            format!("--{} names the key ", field.flag).as_bytes(),
                            &key,
                            b" twice",
                        ]
                        .concat(),
                    ));
                }
                slot.keys.push(key.clone());
                let mut entry = key;
                entry.push(b':');
                entry.extend_from_slice(&json_text(text.get(split + 1..).unwrap_or_default()));
                slot.append(&entry);
            }
            Label::Singular | Label::Optional | Label::Oneof => {
                if slot.set {
                    return Err(Stop::usage(format!("--{} is given twice", field.flag)));
                }
                slot.vals = encode(field, text)?;
                slot.set = true;
            }
        }
        Ok(())
    }

    /// The error for a secret given twice, by file and stdin or either one twice.
    fn secret_twice(field: &Field) -> Stop {
        Stop::usage(format!(
            "--{0}-file and --{0}-stdin: give one, once",
            field.flag
        ))
    }

    /// Takes the field flag that starts `args`; how many arguments it took, or `None` when
    /// `args` do not start with a flag of a field.
    pub(crate) fn take(&mut self, globals: &mut Globals, args: &[Vec<u8>]) -> Flow<Option<usize>> {
        let first = args.first().map(Vec::as_slice).unwrap_or_default();
        let flag = flag_of(first);
        let fields = self.fields;
        for (at, field) in fields.iter().enumerate() {
            match field.input {
                Input::RequestFile => {}
                Input::Flag => {
                    if flag == format!("--{}", field.flag).as_bytes() {
                        let (value, took) = value_of(args)?;
                        self.put(at, field, value)?;
                        return Ok(Some(took));
                    }
                }
                Input::SecretFile => {
                    if flag == format!("--{}-file", field.flag).as_bytes() {
                        let (value, took) = value_of(args)?;
                        self.take_secret(at, field, value, None)?;
                        return Ok(Some(took));
                    }
                    if flag == format!("--{}-stdin", field.flag).as_bytes() {
                        let took = switch(args)?;
                        self.take_secret(at, field, b"-", Some((globals, flag)))?;
                        return Ok(Some(took));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Records where the secret of `field`, the `at`th, is read from; a secret is given once. A
    /// secret read from stdin (`stdin` is the flag that says so) claims it.
    fn take_secret(
        &mut self,
        at: usize,
        field: &Field,
        source: &[u8],
        stdin: Option<(&mut Globals, &[u8])>,
    ) -> Flow<()> {
        let slot = &mut self.slots[at];
        if slot.set {
            return Err(Self::secret_twice(field));
        }
        if let Some((globals, flag)) = stdin {
            globals.claim_stdin(flag)?;
        }
        slot.source = source.to_vec();
        slot.set = true;
        Ok(())
    }
}

impl Slot {
    /// Adds `json` to the values, after a `,` when there are some.
    fn append(&mut self, json: &[u8]) {
        if self.set {
            self.vals.push(b',');
        }
        self.vals.extend_from_slice(json);
        self.set = true;
    }
}
