//! In-memory ManageSieve server for hermetic tests of the deployer.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::SieveOps;

#[derive(Debug, Clone, Default)]
pub struct FakeSieveOps {
    pub scripts: BTreeMap<String, String>,
    pub active: Option<String>,
    pub put_log: Vec<String>,
}

impl FakeSieveOps {
    pub fn new() -> FakeSieveOps {
        FakeSieveOps::default()
    }

    /// Seed an existing (optionally active) script.
    pub fn with_script(mut self, name: &str, content: &str, active: bool) -> FakeSieveOps {
        self.scripts.insert(name.to_string(), content.to_string());
        if active {
            self.active = Some(name.to_string());
        }
        self
    }
}

impl SieveOps for FakeSieveOps {
    fn active_script(&mut self) -> Result<Option<String>> {
        Ok(self.active.clone())
    }

    fn get_script(&mut self, name: &str) -> Result<Option<String>> {
        Ok(self.scripts.get(name).cloned())
    }

    fn put_script(&mut self, name: &str, content: &str) -> Result<()> {
        // Mimic the server's PUTSCRIPT validation: a sieve using fileinto must require it.
        if content.contains("fileinto") && !content.contains("require") {
            bail!("PUTSCRIPT rejected: fileinto used without require");
        }
        self.scripts.insert(name.to_string(), content.to_string());
        self.put_log.push(name.to_string());
        Ok(())
    }

    fn set_active(&mut self, name: &str) -> Result<()> {
        if !self.scripts.contains_key(name) {
            bail!("SETACTIVE of nonexistent script {name}");
        }
        self.active = Some(name.to_string());
        Ok(())
    }
}
