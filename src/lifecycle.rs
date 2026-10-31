//! Durable lifecycle and exclusive restore claims, independent of terminal APIs.
use crate::sessions::{Session, Store, canonical_terminal_key, list_sessions, persist_session};
use crate::{Result, invalid};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub session: Session,
    pub shell_key: String,
    pub owner_key: String,
    pub executable: PathBuf,
    pub options: Vec<String>,
    pub phase: String,
    pub reason: String,
    pub claim_shell: String,
}

#[derive(Clone, Debug)]
pub struct Capture {
    pub session: Session,
    pub shell_key: String,
    pub owner_key: String,
    pub executable: PathBuf,
    pub options: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Nothing,
    Retired,
    Launch,
}

fn valid_phase(phase: &str) -> Result<()> {
    if !matches!(
        phase,
        "active" | "claim" | "attention" | "retired" | "retry"
    ) {
        return Err(invalid(
            "unknown lifecycle state; refusing automatic recovery",
        ));
    }
    Ok(())
}

impl Store {
    pub fn bindings(&self) -> Result<Vec<Binding>> {
        let Some(db) = self.connect(false)? else {
            return Ok(Vec::new());
        };
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 2 {
            return Ok(Vec::new());
        }
        let mut bindings = Vec::new();
        for session in list_sessions(&db)? {
            let row = db.query_row("SELECT shell_key,owner_key,executable,options_json,phase,reason,claim_shell FROM runtime WHERE terminal_key=?1",
                [&session.terminal_key], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?))).optional()?;
            if let Some((shell_key, owner_key, executable, options, phase, reason, claim_shell)) =
                row
            {
                valid_phase(&phase)?;
                let options: Vec<String> = serde_json::from_str(&options)?;
                crate::agents::validate_options(session.agent, &options)?;
                let executable = PathBuf::from(executable);
                if !executable.is_absolute() {
                    return Err(invalid("saved executable is not absolute"));
                }
                bindings.push(Binding {
                    session,
                    shell_key,
                    owner_key,
                    executable,
                    options,
                    phase,
                    reason,
                    claim_shell,
                });
            }
        }
        Ok(bindings)
    }

    pub fn capture(&self, capture: &Capture) -> Result<()> {
        self.capture_observed(capture, None)
    }

    pub fn capture_observed(&self, capture: &Capture, tick: Option<u64>) -> Result<()> {
        capture.session.validate()?;
        let terminal_key = canonical_terminal_key(&capture.session.terminal_key);
        crate::agents::validate_options(capture.session.agent, &capture.options)?;
        if !capture.executable.is_absolute() {
            return Err(invalid("agent executable must be absolute"));
        }
        let mut db = self
            .connect(true)?
            .ok_or_else(|| invalid("state unavailable"))?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let retired_owner: Option<String> = tx
            .query_row(
                "SELECT owner_key FROM runtime WHERE terminal_key=?1 AND phase='retired'",
                [&terminal_key],
                |r| r.get(0),
            )
            .optional()?;
        if retired_owner.as_deref() == Some(&capture.owner_key) {
            return Ok(());
        }
        if let Some(tick) = tick
            && !accept_observation(&tx, &terminal_key, &capture.owner_key, tick)?
        {
            return Ok(());
        }
        let expected=tx.query_row("SELECT s.agent,s.session_id,s.session_file FROM runtime r JOIN sessions s ON s.terminal_key=r.terminal_key WHERE r.terminal_key=?1 AND r.phase IN ('claim','attention') AND r.claim_shell<>''",[&terminal_key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?))).optional()?;
        if expected.is_some_and(|(agent, id, file)| {
            agent != capture.session.agent.name()
                || id != capture.session.session_id.to_string()
                || file.as_deref()
                    != capture
                        .session
                        .session_file
                        .as_ref()
                        .and_then(|p| p.to_str())
        }) {
            tx.execute("UPDATE runtime SET phase='attention',reason='The launched agent reported a different conversation. Saved identity was preserved.' WHERE terminal_key=?1",[&terminal_key])?;
            tx.commit()?;
            return Err(invalid("restored conversation did not match its claim"));
        }
        persist_session(&tx, &capture.session)?;
        tx.execute("INSERT INTO runtime (terminal_key,shell_key,owner_key,executable,options_json,phase,reason,seen_at)
            VALUES (?1,?2,?3,?4,?5,'active','',unixepoch()) ON CONFLICT(terminal_key) DO UPDATE SET
            shell_key=excluded.shell_key,owner_key=excluded.owner_key,executable=excluded.executable,
            options_json=excluded.options_json,phase='active',reason='',claim_shell='',seen_at=unixepoch()",
            params![terminal_key,capture.shell_key,capture.owner_key,capture.executable.to_str(),serde_json::to_string(&capture.options)?])?;
        tx.commit()?;
        Ok(())
    }

    pub fn prepare(&self, key: &str, shell: &str) -> Result<Decision> {
        let mut db = self
            .connect(true)?
            .ok_or_else(|| invalid("state unavailable"))?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = tx
            .query_row(
                "SELECT shell_key,phase,claim_shell FROM runtime WHERE terminal_key=?1",
                [key],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((_, phase, _)) = &state {
            valid_phase(phase)?;
        }
        let decision = match state {
            None => Decision::Nothing,
            Some((_, phase, _)) if matches!(phase.as_str(), "retired" | "attention") => {
                Decision::Nothing
            }
            Some((_, phase, claim)) if phase == "claim" => {
                if claim != shell {
                    tx.execute("UPDATE runtime SET phase='attention',reason='Restoration was interrupted. Run ghostback retry, then press Enter in this terminal.' WHERE terminal_key=?1", [key])?;
                }
                Decision::Nothing
            }
            Some((origin, phase, _)) if phase != "retry" && origin == shell => {
                tx.execute("UPDATE runtime SET phase='retired',reason='Agent exited in its original shell.' WHERE terminal_key=?1", [key])?;
                Decision::Retired
            }
            Some((_, phase, _)) => {
                valid_phase(&phase)?;
                tx.execute("UPDATE runtime SET phase='claim',claim_shell=?2,reason='Waiting for exact conversation acknowledgement.' WHERE terminal_key=?1", params![key,shell])?;
                Decision::Launch
            }
        };
        tx.commit()?;
        Ok(decision)
    }

    pub fn clear_selection(&self, key: &str, owner: &str, tick: u64) -> Result<()> {
        let mut db = self
            .connect(true)?
            .ok_or_else(|| invalid("state unavailable"))?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !accept_observation(&tx, key, owner, tick)? {
            return Ok(());
        }
        tx.execute("UPDATE runtime SET phase='attention',reason='No persistent conversation is selected. Select a saved conversation in the agent.' WHERE terminal_key=?1 AND phase<>'retired'", [key])?;
        tx.commit()?;
        Ok(())
    }

    pub fn attention_if_unchanged(
        &self,
        key: &str,
        owner: &str,
        phase: &str,
        reason: &str,
    ) -> Result<()> {
        let db = self
            .connect(true)?
            .ok_or_else(|| invalid("state unavailable"))?;
        db.execute("UPDATE runtime SET phase='attention',reason=?4 WHERE terminal_key=?1 AND owner_key=?2 AND phase=?3 AND phase<>'retired'",params![key,owner,phase,reason])?;
        Ok(())
    }

    pub fn attention(&self, key: &str, reason: &str) -> Result<()> {
        let db = self
            .connect(true)?
            .ok_or_else(|| invalid("state unavailable"))?;
        db.execute(
            "UPDATE runtime SET phase='attention',reason=?2 WHERE terminal_key=?1",
            params![key, reason],
        )?;
        Ok(())
    }

    pub fn retire(&self, key: &str, reason: &str) -> Result<()> {
        let db = self
            .connect(true)?
            .ok_or_else(|| invalid("state unavailable"))?;
        db.execute(
            "UPDATE runtime SET phase='retired',reason=?2 WHERE terminal_key=?1",
            params![key, reason],
        )?;
        Ok(())
    }

    pub fn retry(&self, key: Option<&str>) -> Result<usize> {
        let key = key.map(canonical_terminal_key);
        let db = self
            .connect(true)?
            .ok_or_else(|| invalid("state unavailable"))?;
        Ok(db.execute("UPDATE runtime SET phase='retry',reason='Retry armed. Press Enter in the matching terminal.' WHERE (?1 IS NULL OR terminal_key=?1) AND phase IN ('attention','claim')", [key.as_deref()])?)
    }

    pub fn health(&self, key: &str, value: &str) -> Result<()> {
        let db = self
            .connect(true)?
            .ok_or_else(|| invalid("state unavailable"))?;
        db.execute(
            "INSERT INTO health VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE health.value<>excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn health_status(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let Some(db) = self.connect(false)? else {
            return Ok(Default::default());
        };
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 2 {
            return Ok(Default::default());
        }
        let mut statement = db.prepare("SELECT key,value FROM health ORDER BY key")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

// Monotonic UI observations prevent a slow hook from reviving an older selection.
fn accept_observation(
    tx: &rusqlite::Transaction<'_>,
    key: &str,
    owner: &str,
    tick: u64,
) -> Result<bool> {
    let previous = tx
        .query_row(
            "SELECT owner_key,tick FROM observations WHERE terminal_key=?1",
            [key],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((previous_owner, previous_tick)) = previous
        && previous_owner == owner
        && previous_tick.parse::<u64>()? >= tick
    {
        return Ok(false);
    }
    tx.execute("INSERT INTO observations (terminal_key,owner_key,tick) VALUES (?1,?2,?3) ON CONFLICT(terminal_key) DO UPDATE SET owner_key=excluded.owner_key,tick=excluded.tick", params![key,owner,tick.to_string()])?;
    Ok(true)
}
