use std::collections::HashSet;

use super::Manager;

impl Manager {
    /// Compute which glitch-free defs are affected by writes to given vars
    pub fn compute_affected_glitchfree(&self, written_vars: &HashSet<String>) -> HashSet<String> {
        let mut affected = HashSet::new();
        
        // For each glitch-free def
        for def_name in &self.glitchfree_defs {
            // Check if it transitively depends on any written var
            if let Some(transitive_vars) = self.dep_tran_vars.get(def_name) {
                if !transitive_vars.is_disjoint(written_vars) {
                    // This glitch-free def depends on a written var
                    affected.insert(def_name.clone());
                }
            }
        }
        
        affected
    }
}
