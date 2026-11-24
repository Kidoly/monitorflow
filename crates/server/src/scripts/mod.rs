//! The script library and automation rules (ADR-0179).
//!
//! A script is shell or PowerShell text with typed parameters, kept in
//! versions. A run sends one version to servers as signed `run_script`
//! commands (ADR-0031), which agents run as their own user only when the
//! host's administrator allowed it (`commands.scripts`). Every run is
//! classified by the risk rules of AI commands (ADR-0041), line by line:
//! a forbidden line refuses the whole script, and anything the rules do
//! not know is high impact.
//!
//! - [`store`]: the tables.
//! - [`run`]: targets, classification, approval and dispatch of runs.
//! - [`automation`]: rules that start runs from events.
//! - [`ai`]: library scripts as ready-made fixes for the AI remediation
//!   loop.
//! - [`worker`]: the task that dispatches due runs, expires unanswered
//!   approvals and fires rules.

pub mod ai;
pub mod automation;
pub mod run;
pub mod store;
pub mod worker;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::remediation::risk::{self, Decision, Rule, RuleSet, Shell, Tier};

/// Longest script, in bytes; the agent refuses longer ones.
pub const MAX_BODY: usize = 64 * 1024;
/// Most parameters of one script.
pub const MAX_PARAMETERS: usize = 20;
/// Longest parameter value, in characters.
pub const MAX_VALUE: usize = 256;
/// Longest description, in characters.
pub const MAX_DESCRIPTION: usize = 2000;
/// Longest version note, in characters.
pub const MAX_NOTE: usize = 500;
/// Most choices of a `choice` parameter.
pub const MAX_CHOICES: usize = 50;
/// What the AI writes to propose a library script.
pub const AI_PREFIX: &str = "mf-script";

/// The program that runs a script, which decides the risk rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Interpreter {
    /// `/bin/sh -c`, on Linux.
    Sh,
    /// Windows PowerShell 5.1, on Windows.
    #[serde(rename = "powershell")]
    PowerShell,
}

impl Interpreter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::PowerShell => "powershell",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Self::Sh, Self::PowerShell]
            .into_iter()
            .find(|i| i.as_str() == text)
    }

    /// The shell whose rules classify it.
    pub fn shell(self) -> Shell {
        match self {
            Self::Sh => Shell::Posix,
            Self::PowerShell => Shell::PowerShell,
        }
    }

    /// Whether a server of `os_family` runs it.
    pub fn fits(self, os_family: &str) -> bool {
        Shell::of(os_family) == self.shell()
    }
}

/// The type of a parameter's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParameterType {
    /// One line of text without control characters.
    String,
    /// A whole number, written in decimal.
    Integer,
    /// `true` or `false`.
    Boolean,
    /// One of `choices`.
    Choice,
}

/// A parameter of a script version. Its value reaches the script as the
/// environment variable `MF_PARAM_<NAME>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ScriptParameter)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    /// `[a-z][a-z0-9_]{0,31}`.
    pub name: String,
    /// What the run form shows; the name when empty.
    #[serde(default)]
    pub label: String,
    #[serde(rename = "type")]
    pub kind: ParameterType,
    /// A run must give a value unless there is a default.
    #[serde(default)]
    pub required: bool,
    /// Used when a run gives none.
    #[serde(default)]
    pub default: Option<String>,
    /// The values of a `choice` parameter.
    #[serde(default)]
    pub choices: Vec<String>,
}

/// Whether `name` names a script: `[a-z0-9][a-z0-9_.-]{0,62}`.
pub fn valid_script_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 63
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c))
}

/// Whether `name` names a parameter, as the agent checks it.
pub fn valid_parameter_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Why `body` cannot be a script, if it cannot.
pub fn check_body(body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("The script is empty.".into());
    }
    if body.len() > MAX_BODY {
        return Err(format!("The script must be at most {MAX_BODY} bytes."));
    }
    if body.contains('\0') {
        return Err("The script must not contain NUL characters.".into());
    }
    Ok(())
}

/// Why `value` is not a value of `parameter`, if it is not.
fn check_value(parameter: &Parameter, value: &str) -> Result<(), String> {
    let name = &parameter.name;
    if value.chars().count() > MAX_VALUE || value.chars().any(char::is_control) {
        return Err(format!(
            "`{name}` must be one line of at most {MAX_VALUE} characters."
        ));
    }
    match parameter.kind {
        ParameterType::String => Ok(()),
        ParameterType::Integer if value.parse::<i64>().is_ok() => Ok(()),
        ParameterType::Integer => Err(format!("`{name}` must be a whole number.")),
        ParameterType::Boolean if value == "true" || value == "false" => Ok(()),
        ParameterType::Boolean => Err(format!("`{name}` must be true or false.")),
        ParameterType::Choice if parameter.choices.iter().any(|c| c == value) => Ok(()),
        ParameterType::Choice => Err(format!(
            "`{name}` must be one of: {}.",
            parameter.choices.join(", ")
        )),
    }
}

/// Checks the parameters of a version: names, labels, choices, defaults.
pub fn check_parameters(parameters: &[Parameter]) -> Result<(), String> {
    if parameters.len() > MAX_PARAMETERS {
        return Err(format!("A script has at most {MAX_PARAMETERS} parameters."));
    }
    for (i, parameter) in parameters.iter().enumerate() {
        let name = &parameter.name;
        if !valid_parameter_name(name) {
            return Err(format!(
                "`{}` is not a parameter name: lower case letters, digits and _, starting with \
                 a letter, at most 32.",
                name.escape_debug()
            ));
        }
        if parameters[..i].iter().any(|p| p.name == *name) {
            return Err(format!("The parameter `{name}` is there twice."));
        }
        if parameter.label.chars().count() > 80 || parameter.label.chars().any(char::is_control) {
            return Err(format!(
                "The label of `{name}` must be one line of at most 80 characters."
            ));
        }
        match parameter.kind {
            ParameterType::Choice if parameter.choices.is_empty() => {
                return Err(format!("Give `{name}` its choices."));
            }
            ParameterType::Choice if parameter.choices.len() > MAX_CHOICES => {
                return Err(format!("`{name}` has at most {MAX_CHOICES} choices."));
            }
            ParameterType::Choice => {
                for choice in &parameter.choices {
                    let probe = Parameter {
                        kind: ParameterType::String,
                        ..parameter.clone()
                    };
                    check_value(&probe, choice)?;
                    if choice.is_empty() {
                        return Err(format!("The choices of `{name}` must not be empty."));
                    }
                }
            }
            _ if !parameter.choices.is_empty() => {
                return Err(format!("Only a choice parameter has choices (`{name}`)."));
            }
            _ => {}
        }
        if let Some(default) = &parameter.default {
            check_value(parameter, default)?;
        }
    }
    Ok(())
}

/// The value of each parameter for a run, from `given` and the defaults,
/// in the parameters' order; parameters without a value are left out.
pub fn values(
    parameters: &[Parameter],
    given: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    if let Some(unknown) = given
        .keys()
        .find(|k| !parameters.iter().any(|p| &p.name == *k))
    {
        return Err(format!(
            "The script has no parameter `{}`.",
            unknown.escape_debug()
        ));
    }
    let mut values = Vec::new();
    for parameter in parameters {
        let value = given
            .get(&parameter.name)
            .filter(|v| !v.is_empty())
            .or(parameter.default.as_ref());
        match value {
            Some(value) => {
                check_value(parameter, value)?;
                values.push((parameter.name.clone(), value.clone()));
            }
            None if parameter.required => {
                return Err(format!("Give a value for `{}`.", parameter.name));
            }
            None => {}
        }
    }
    Ok(values)
}

fn decision(tier: Tier, id: &'static str, reason: String) -> Decision {
    Decision {
        tier,
        rule: Rule::Builtin { id },
        reason,
    }
}

/// Classifies a script and its parameter values by the rules of AI
/// commands (ADR-0041, ADR-0084): each line that is not empty or a comment
/// as one command line of `interpreter`, the script taking the highest
/// tier, a forbidden line refusing it. A value matching a deny pattern
/// refuses it too, though values never reach the script's text.
pub fn classify(
    interpreter: Interpreter,
    body: &str,
    values: &[(String, String)],
    rules: &RuleSet,
) -> Decision {
    let shell = interpreter.shell();
    for (name, value) in values {
        let verdict = risk::classify_for(shell, value, rules);
        if verdict.tier == Tier::Forbidden {
            return Decision {
                reason: format!("parameter `{name}`: {}", verdict.reason),
                ..verdict
            };
        }
    }
    let mut worst: Option<(usize, Decision)> = None;
    for (number, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let verdict = risk::classify_for(shell, line, rules);
        if verdict.tier == Tier::Forbidden {
            return Decision {
                reason: format!("line {}: {}", number + 1, verdict.reason),
                ..verdict
            };
        }
        if worst.as_ref().is_none_or(|(_, w)| verdict.tier > w.tier) {
            worst = Some((number + 1, verdict));
        }
    }
    match worst {
        Some((_, verdict)) if verdict.tier == Tier::ReadOnly => decision(
            Tier::ReadOnly,
            "script.read_only",
            "every line of the script is read-only".into(),
        ),
        Some((number, verdict)) => Decision {
            reason: format!("line {number}: {}", verdict.reason),
            ..verdict
        },
        None => decision(
            Tier::HighImpact,
            "script.empty",
            "the script has only comments".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter(name: &str, kind: ParameterType) -> Parameter {
        Parameter {
            name: name.into(),
            label: String::new(),
            kind,
            required: false,
            default: None,
            choices: Vec::new(),
        }
    }

    #[test]
    fn names() {
        assert!(valid_script_name("clear-cache") && valid_script_name("a.b_c-1"));
        for bad in ["", "-a", "A", "a b", "é", &"a".repeat(64)] {
            assert!(!valid_script_name(bad), "{bad}");
        }
        assert!(valid_parameter_name("max_age"));
        assert!(!valid_parameter_name("1a") && !valid_parameter_name("a-b"));
    }

    #[test]
    fn parameters_are_checked() {
        let mut choice = parameter("mode", ParameterType::Choice);
        assert!(check_parameters(std::slice::from_ref(&choice)).is_err());
        choice.choices = vec!["fast".into(), "full".into()];
        choice.default = Some("full".into());
        let count = Parameter {
            default: Some("3".into()),
            ..parameter("count", ParameterType::Integer)
        };
        check_parameters(&[choice.clone(), count.clone()]).unwrap();
        let mut bad = count.clone();
        bad.default = Some("x".into());
        assert!(check_parameters(&[bad]).is_err());
        assert!(check_parameters(&[count.clone(), count.clone()]).is_err());
        let mut stray = parameter("x", ParameterType::String);
        stray.choices = vec!["a".into()];
        assert!(check_parameters(&[stray]).is_err());
    }

    #[test]
    fn values_come_from_the_run_or_the_defaults() {
        let mut path = parameter("path", ParameterType::String);
        path.required = true;
        let flag = Parameter {
            default: Some("false".into()),
            ..parameter("force", ParameterType::Boolean)
        };
        let optional = parameter("note", ParameterType::String);
        let parameters = [path, flag, optional];
        let given = BTreeMap::from([("path".to_owned(), "/var/tmp".to_owned())]);
        assert_eq!(
            values(&parameters, &given).unwrap(),
            [
                ("path".to_owned(), "/var/tmp".to_owned()),
                ("force".to_owned(), "false".to_owned())
            ]
        );
        assert!(values(&parameters, &BTreeMap::new()).is_err());
        let unknown = BTreeMap::from([("other".to_owned(), "1".to_owned())]);
        assert!(values(&parameters, &unknown).is_err());
        let bad = BTreeMap::from([
            ("path".to_owned(), "a".to_owned()),
            ("force".to_owned(), "yes".to_owned()),
        ]);
        assert!(values(&parameters, &bad).is_err());
        let multiline = BTreeMap::from([("path".to_owned(), "a\nb".to_owned())]);
        assert!(values(&parameters, &multiline).is_err());
    }

    #[test]
    fn scripts_take_the_highest_tier_of_their_lines() {
        let rules = RuleSet::default();
        let read_only = classify(
            Interpreter::Sh,
            "# disk and memory\ndf -h\n\nfree -m\n",
            &[],
            &rules,
        );
        assert_eq!(read_only.tier, Tier::ReadOnly);
        assert_eq!(
            read_only.rule,
            Rule::Builtin {
                id: "script.read_only"
            }
        );
        let high = classify(
            Interpreter::Sh,
            "df -h\nfor x in a; do echo $x; done\n",
            &[],
            &rules,
        );
        assert_eq!(high.tier, Tier::HighImpact);
        assert!(high.reason.starts_with("line 2: "), "{}", high.reason);
        let forbidden = classify(Interpreter::Sh, "df -h\nrm -rf /\n", &[], &rules);
        assert_eq!(forbidden.tier, Tier::Forbidden);
        assert!(
            forbidden.reason.starts_with("line 2: "),
            "{}",
            forbidden.reason
        );
        let empty = classify(Interpreter::Sh, "# nothing\n", &[], &rules);
        assert_eq!(empty.tier, Tier::HighImpact);
        // A value matching a deny pattern refuses the script.
        let values = [("target".to_owned(), "rm -rf /".to_owned())];
        let refused = classify(Interpreter::Sh, "df -h", &values, &rules);
        assert_eq!(refused.tier, Tier::Forbidden);
        assert!(refused.reason.starts_with("parameter `target`"));
        // PowerShell scripts take PowerShell's rules.
        let windows = classify(Interpreter::PowerShell, "Get-Service\n", &[], &rules);
        assert_eq!(windows.tier, Tier::ReadOnly);
        assert_eq!(
            classify(Interpreter::Sh, "Get-Service", &[], &rules).tier,
            Tier::HighImpact
        );
    }

    #[test]
    fn interpreters_fit_their_systems() {
        assert!(Interpreter::Sh.fits("debian") && !Interpreter::Sh.fits("windows"));
        assert!(Interpreter::PowerShell.fits("windows"));
        assert_eq!(
            Interpreter::parse("powershell"),
            Some(Interpreter::PowerShell)
        );
        assert_eq!(Interpreter::parse("bash"), None);
    }
}
