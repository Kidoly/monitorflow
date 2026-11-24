//! Library scripts as ready-made fixes for the AI (ADR-0189).
//!
//! The prompt lists the scripts marked for AI use that fit the server's
//! system, with their parameters. The AI proposes one as its command,
//! `mf-script <name> [name=value ...]`, and the remediation loop treats
//! it as any command: classified by the rules (the script's lines and the
//! values), run at once, approved or refused alike, counted in the
//! incident's limits, its output given back to the AI. It is sent as a
//! `run_script` command, which the agent runs only when its host allows
//! library scripts. The AI's words are untrusted: a name it may not use
//! is refused, never guessed.

use std::collections::BTreeMap;

use sqlx::PgExecutor;
use uuid::Uuid;

use super::store::{self, Script};
use super::{classify, values, Parameter, ParameterType, AI_PREFIX};
use crate::remediation::risk::{self, Decision, Rule, RuleSet, Shell, Tier};

/// Most scripts listed in a prompt.
pub const MAX_OFFERED: usize = 30;

/// The library scripts the AI may propose on a server of `customer`
/// running `shell`: marked for AI use, of every customer or this one (its
/// own winning over one of every customer with the same name).
pub async fn offered(
    db: impl PgExecutor<'_>,
    customer: Uuid,
    shell: Shell,
) -> Result<Vec<Script>, sqlx::Error> {
    let scripts = store::list(db, false, &[customer]).await?;
    let mut by_name: BTreeMap<String, Script> = BTreeMap::new();
    for script in scripts
        .into_iter()
        .filter(|s| s.ai_allowed && s.interpreter.shell() == shell)
    {
        let own = script.customer_id.is_some();
        match by_name.get(&script.name) {
            Some(existing) if existing.customer_id.is_some() || !own => {}
            _ => {
                by_name.insert(script.name.clone(), script);
            }
        }
    }
    Ok(by_name.into_values().take(MAX_OFFERED).collect())
}

/// What the prompt says of `scripts`; empty without any.
pub fn describe(scripts: &[Script]) -> String {
    if scripts.is_empty() {
        return String::new();
    }
    let mut text = format!(
        "\nLibrary scripts:\n- Instead of a command, you may propose one of these scripts, \
written and reviewed by the administrators, as the whole command: \
{AI_PREFIX} <name> [parameter=value ...]. Values have no spaces. MonitorFlow \
classifies the script like any command and runs it as the agent's user.\n"
    );
    for script in scripts {
        let description = one_line(&script.description, 300);
        text.push_str(&format!("- {}: {description}", script.name));
        if !script.parameters.is_empty() {
            let parameters: Vec<String> = script.parameters.iter().map(parameter).collect();
            text.push_str(&format!(" Parameters: {}.", parameters.join("; ")));
        }
        text.push('\n');
    }
    text
}

fn parameter(p: &Parameter) -> String {
    let kind = match p.kind {
        ParameterType::String => "text".to_owned(),
        ParameterType::Integer => "whole number".to_owned(),
        ParameterType::Boolean => "true or false".to_owned(),
        ParameterType::Choice => format!("one of {}", p.choices.join(", ")),
    };
    let mut text = format!("{} ({kind}", p.name);
    if p.required && p.default.is_none() {
        text.push_str(", required");
    }
    if let Some(default) = &p.default {
        text.push_str(&format!(", default {default}"));
    }
    text.push(')');
    text
}

/// `text` on one line, cut to `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        line
    } else {
        format!("{}...", line.chars().take(max).collect::<String>())
    }
}

/// A script call the AI wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub name: String,
    pub given: BTreeMap<String, String>,
}

/// `command` as a script call: `None` when it is not one, else the call
/// or why it cannot be read.
pub fn parse(command: &str) -> Option<Result<Call, String>> {
    let mut words = command.split_whitespace();
    if words.next() != Some(AI_PREFIX) {
        return None;
    }
    let Some(name) = words.next() else {
        return Some(Err(format!("`{AI_PREFIX}` needs a script name")));
    };
    let mut given = BTreeMap::new();
    for word in words {
        let Some((key, value)) = word.split_once('=') else {
            return Some(Err(format!(
                "`{}` is not parameter=value",
                word.escape_debug()
            )));
        };
        if given.insert(key.to_owned(), value.to_owned()).is_some() {
            return Some(Err(format!("`{}` is given twice", key.escape_debug())));
        }
    }
    Some(Ok(Call {
        name: name.to_owned(),
        given,
    }))
}

/// A call resolved to the script and the values that would run.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub script: Script,
    pub values: Vec<(String, String)>,
}

/// The script `call` names among those offered on a server of `customer`
/// running `shell`, with its values; or why it cannot run, in words for
/// the audit log.
pub async fn resolve(
    db: impl PgExecutor<'_>,
    customer: Uuid,
    shell: Shell,
    call: Result<Call, String>,
) -> Result<Result<Resolved, String>, sqlx::Error> {
    let call = match call {
        Ok(call) => call,
        Err(why) => return Ok(Err(format!("not a valid script call: {why}"))),
    };
    let offered = offered(db, customer, shell).await?;
    let Some(script) = offered.into_iter().find(|s| s.name == call.name) else {
        return Ok(Err(format!(
            "`{}` is not a library script the AI may use on this server",
            call.name.escape_debug()
        )));
    };
    Ok(match values(&script.parameters, &call.given) {
        Ok(values) => Ok(Resolved { script, values }),
        Err(why) => Err(format!("{}: {why}", script.name)),
    })
}

/// The rules' decision on `command`, a command the AI proposed, and the
/// library script it calls, if it is one: the script's lines and values
/// are classified (see [`super::classify`]), and a script the AI may not
/// use on a server of `customer` running `shell` is forbidden.
pub async fn classify_command(
    db: &sqlx::PgPool,
    customer: Uuid,
    shell: Shell,
    command: &str,
    rules: &RuleSet,
) -> Result<(Decision, Option<Resolved>), sqlx::Error> {
    let Some(call) = parse(command) else {
        return Ok((risk::classify_for(shell, command, rules), None));
    };
    Ok(match resolve(db, customer, shell, call).await? {
        Ok(resolved) => {
            let script = &resolved.script;
            let decision = classify(script.interpreter, &script.body, &resolved.values, rules);
            (decision, Some(resolved))
        }
        Err(why) => (
            Decision {
                tier: Tier::Forbidden,
                rule: Rule::Builtin {
                    id: "script.unknown",
                },
                reason: why,
            },
            None,
        ),
    })
}

/// The `run_script` payload of `resolved`, its timeout at most `limit`.
pub fn payload(resolved: &Resolved, limit: std::time::Duration) -> Vec<u8> {
    use monitorflow_common::proto::v1::{RunScript, ScriptParameter};
    use prost::Message;

    let script = &resolved.script;
    let own = u64::try_from(script.timeout_s).unwrap_or(600);
    RunScript {
        interpreter: script.interpreter.as_str().into(),
        body: script.body.clone(),
        parameters: resolved
            .values
            .iter()
            .map(|(name, value)| ScriptParameter {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        timeout_seconds: u32::try_from(own.min(limit.as_secs())).unwrap_or(u32::MAX),
        script_id: script.id.to_string(),
        version: u32::try_from(script.version).unwrap_or_default(),
    }
    .encode_to_vec()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::api::Role;
    use crate::scripts::Interpreter;

    fn script(name: &str, parameters: Vec<Parameter>) -> Script {
        Script {
            id: Uuid::nil(),
            customer_id: None,
            name: name.into(),
            description: "Clears the\napplication cache.".into(),
            interpreter: Interpreter::Sh,
            run_role: Role::Operator,
            ai_allowed: true,
            timeout_s: 60,
            version: 1,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
            body: "df -h".into(),
            parameters,
        }
    }

    #[test]
    fn calls_are_read_strictly() {
        assert_eq!(parse("df -h"), None);
        assert_eq!(parse("mf-scriptx a"), None);
        assert_eq!(
            parse("mf-script clear-cache days=3 force=true"),
            Some(Ok(Call {
                name: "clear-cache".into(),
                given: BTreeMap::from([
                    ("days".into(), "3".into()),
                    ("force".into(), "true".into())
                ]),
            }))
        );
        assert!(parse("mf-script").unwrap().is_err());
        assert!(parse("mf-script a b").unwrap().is_err());
        assert!(parse("mf-script a b=1 b=2").unwrap().is_err());
    }

    #[test]
    fn the_prompt_lists_scripts_and_parameters() {
        assert_eq!(describe(&[]), "");
        let days = Parameter {
            name: "days".into(),
            label: String::new(),
            kind: ParameterType::Integer,
            required: true,
            default: Some("7".into()),
            choices: Vec::new(),
        };
        let text = describe(&[script("clear-cache", vec![days])]);
        assert!(text.contains("mf-script <name>"), "{text}");
        assert!(
            text.contains("- clear-cache: Clears the application cache. Parameters: days (whole number, default 7)."),
            "{text}"
        );
    }
}
