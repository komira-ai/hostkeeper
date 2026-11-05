//! The command line of a step: `<step> [<artifact>] [--params <file>]`.

use std::ffi::OsString;
use std::path::PathBuf;

/// What the updater asks of the hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Prepare,
    Cutover,
    Verify,
    Rollback,
    Cleanup,
    Uninstall,
}

impl Step {
    fn from_word(word: &str) -> Option<Step> {
        Some(match word {
            "prepare" => Step::Prepare,
            "cutover" => Step::Cutover,
            "verify" => Step::Verify,
            "rollback" => Step::Rollback,
            "cleanup" => Step::Cleanup,
            "uninstall" => Step::Uninstall,
            _ => return None,
        })
    }
}

/// A parsed step command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub step: Step,
    /// The staged release directory: given to `prepare` and to no other step.
    pub artifact: Option<PathBuf>,
    pub params: Option<PathBuf>,
}

impl Call {
    /// Parses the arguments from the step on. The error is the text for the report.
    pub fn parse(args: &[OsString]) -> Result<Call, String> {
        let mut args = args.iter();
        let first = args.next().ok_or("no step given")?;
        let step = first
            .to_str()
            .and_then(Step::from_word)
            .ok_or_else(|| format!("unknown step {first:?}"))?;
        let mut artifact = None;
        let mut params = None;
        while let Some(arg) = args.next() {
            if arg == "--params" {
                let file = args.next().ok_or("--params needs a file")?;
                if params.replace(PathBuf::from(file)).is_some() {
                    return Err("--params given twice".to_owned());
                }
            } else if step == Step::Prepare && artifact.is_none() {
                artifact = Some(PathBuf::from(arg));
            } else {
                return Err(format!("unexpected argument {arg:?}"));
            }
        }
        if step == Step::Prepare && artifact.is_none() {
            return Err("prepare needs the staged release directory".to_owned());
        }
        Ok(Call {
            step,
            artifact,
            params,
        })
    }
}
