//! `managesieve` — deploy the generated Sieve rules to the server.
//!
//! The [`SieveDeployer`] fetches the account's active Sieve script, merges the tool's
//! managed rule block into it (via [`sieve::merge`], preserving hand-written rules), then
//! uploads and activates the result. The wire protocol ([ManageSieve, RFC 5804]) is
//! abstracted behind [`SieveOps`] so the merge/deploy logic is tested hermetically against
//! [`FakeSieveOps`]; a real backend (behind the `real-sieve` feature) speaks the protocol
//! over STARTTLS.
//!
//! [ManageSieve, RFC 5804]: https://www.rfc-editor.org/rfc/rfc5804

use anyhow::Result;
use model::SieveRule;

/// Default script name used when the account has no active Sieve script yet.
pub const DEFAULT_SCRIPT: &str = "mail-util";

/// The ManageSieve operations the deployer needs. Script names are bare (unquoted).
pub trait SieveOps {
    /// Name of the currently-active script, if any.
    fn active_script(&mut self) -> Result<Option<String>>;
    /// Fetch a script's contents, or `None` if it does not exist.
    fn get_script(&mut self, name: &str) -> Result<Option<String>>;
    /// Upload a script. The server validates it (PUTSCRIPT), so an invalid script errors.
    fn put_script(&mut self, name: &str, content: &str) -> Result<()>;
    /// Make a script the active one.
    fn set_active(&mut self, name: &str) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployReport {
    /// The script that was written and activated.
    pub script: String,
    /// Size of the merged script in bytes.
    pub bytes: usize,
    /// True if the script did not exist before (freshly created).
    pub created: bool,
}

/// Merges the tool's rules into a server-side Sieve script and activates it.
pub struct SieveDeployer<O: SieveOps> {
    ops: O,
}

impl<O: SieveOps> SieveDeployer<O> {
    pub fn new(ops: O) -> SieveDeployer<O> {
        SieveDeployer { ops }
    }

    /// Choose the target script: an explicit `preferred` name, else the active script,
    /// else [`DEFAULT_SCRIPT`].
    fn target(&mut self, preferred: Option<&str>) -> Result<String> {
        if let Some(name) = preferred {
            return Ok(name.to_string());
        }
        Ok(self.ops.active_script()?.unwrap_or_else(|| DEFAULT_SCRIPT.to_string()))
    }

    /// Compute the merged script that would be deployed, without uploading anything.
    pub fn preview(&mut self, preferred: Option<&str>, rules: &[SieveRule]) -> Result<String> {
        let target = self.target(preferred)?;
        let existing = self.ops.get_script(&target)?.unwrap_or_default();
        Ok(sieve::merge(&existing, rules))
    }

    /// Fetch → merge → upload → activate. Returns what was written.
    pub fn deploy(&mut self, preferred: Option<&str>, rules: &[SieveRule]) -> Result<DeployReport> {
        let target = self.target(preferred)?;
        let existing = self.ops.get_script(&target)?;
        let created = existing.is_none();
        let merged = sieve::merge(existing.as_deref().unwrap_or(""), rules);
        self.ops.put_script(&target, &merged)?;
        self.ops.set_active(&target)?;
        Ok(DeployReport {
            script: target,
            bytes: merged.len(),
            created,
        })
    }
}

mod fake;
pub use fake::FakeSieveOps;

#[cfg(feature = "real-sieve")]
mod real;
#[cfg(feature = "real-sieve")]
pub use real::RealSieveOps;

#[cfg(test)]
mod tests;
