//! The values of a plan's parameters, from `--param name=value` (every repetition) and
//! `--param name@k=value` (repetition `k` alone, for a parameter declared `per_repetition`).

use std::collections::BTreeMap;

use thiserror::Error;

use crate::plan::Plan;

/// The parameters of a run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Params {
    every: BTreeMap<String, String>,
    per_rep: BTreeMap<(String, u32), String>,
}

/// Why the parameters do not fit the plan.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParamError {
    #[error("--param {0:?} is not name=value or name@repetition=value")]
    Shape(String),
    #[error("--param {0:?} is given twice")]
    Twice(String),
    #[error("the plan has no parameter {0:?}")]
    Unknown(String),
    #[error("{name:?} differs per repetition: give it as {name}@<repetition>=value")]
    PerRepetition { name: String },
    #[error("{name:?} is the same in every repetition: give it as {name}=value")]
    Every { name: String },
    #[error("repetition {rep} of {name:?}: the plan has repetitions 1 to {reps}")]
    Repetition { name: String, rep: u32, reps: u32 },
    #[error("no value of {name:?} for repetition {rep}")]
    Missing { name: String, rep: u32 },
}

impl Params {
    /// Reads `--param` values.
    pub fn parse(items: &[String]) -> Result<Self, ParamError> {
        let mut out = Self::default();
        for item in items {
            let shape = || ParamError::Shape(item.clone());
            let (key, value) = item.split_once('=').ok_or_else(shape)?;
            let fresh = match key.split_once('@') {
                Some((name, rep)) => {
                    let rep: u32 = rep.parse().map_err(|_| shape())?;
                    out.per_rep
                        .insert((name.to_owned(), rep), value.to_owned())
                        .is_none()
                }
                None => out.every.insert(key.to_owned(), value.to_owned()).is_none(),
            };
            if !fresh {
                return Err(ParamError::Twice(key.to_owned()));
            }
        }
        Ok(out)
    }

    /// Checks that every parameter is the plan's, given as it is declared, for every repetition.
    pub fn check(&self, plan: &Plan) -> Result<(), ParamError> {
        let declared = |name: &str| plan.params.iter().find(|p| p.name == name);
        for name in self.every.keys() {
            let param = declared(name).ok_or_else(|| ParamError::Unknown(name.clone()))?;
            if param.per_repetition {
                return Err(ParamError::PerRepetition { name: name.clone() });
            }
        }
        for (name, rep) in self.per_rep.keys() {
            let param = declared(name).ok_or_else(|| ParamError::Unknown(name.clone()))?;
            if !param.per_repetition {
                return Err(ParamError::Every { name: name.clone() });
            }
            if *rep == 0 || *rep > plan.repetitions {
                return Err(ParamError::Repetition {
                    name: name.clone(),
                    rep: *rep,
                    reps: plan.repetitions,
                });
            }
        }
        for param in &plan.params {
            for rep in 1..=plan.repetitions {
                if self.get(&param.name, rep).is_none() {
                    return Err(ParamError::Missing {
                        name: param.name.clone(),
                        rep,
                    });
                }
            }
        }
        Ok(())
    }

    /// The value of `name` in repetition `rep`.
    pub fn get(&self, name: &str, rep: u32) -> Option<&str> {
        self.per_rep
            .get(&(name.to_owned(), rep))
            .or_else(|| self.every.get(name))
            .map(String::as_str)
    }
}
