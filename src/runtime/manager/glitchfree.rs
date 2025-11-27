use std::collections::HashSet;
use log::info;

use super::Manager;

impl Manager {
    /// Compute which glitch-free defs are affected by writes to given vars
    pub fn compute_affected_glitchfree(&self, written_vars: &HashSet<String>) -> HashSet<String> {
        println!("[DEBUG] Computing affected glitch-free defs for written vars: {:?}", written_vars);
        println!("[DEBUG] Glitch-free defs in system: {:?}", self.glitchfree_defs);
        
        let mut affected = HashSet::new();
        
        // For each glitch-free def
        for def_name in &self.glitchfree_defs {
            // Check if it transitively depends on any written var
            if let Some(transitive_vars) = self.dep_tran_vars.get(def_name) {
                println!("[DEBUG] Checking def '{}': transitive vars = {:?}", def_name, transitive_vars);
                if !transitive_vars.is_disjoint(written_vars) {
                    // This glitch-free def depends on a written var
                    println!("[DEBUG] Def '{}' is affected!", def_name);
                    affected.insert(def_name.clone());
                }
            }
        }
        
        println!("[DEBUG] Affected glitch-free defs: {:?}", affected);
        affected
    }
}
