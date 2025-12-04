use futures::future::Either;
use kameo::actor::ActorRef;
use std::collections::HashMap;
use std::collections::HashSet;

use super::pubsub::PubSub;
use crate::ast::Expr;
use crate::runtime::manager::Manager;
use crate::runtime::transaction::{Txn, TxnId, TxnPred};
use crate::runtime::TestId;
use state::ChangeState;

pub mod handler;
pub mod state;

// pub type TickFunc = Box<
//     dyn for<'a> FnMut(
//             &'a mut DefActor,
//         ) -> Pin<Box
//                 <dyn Future<Output = Result<(), Box<dyn Error + Send>>> + Send + 'a>>
//             >
//             + Send + 'static,
// >;

/// A value with its BasisStamp (for basis checking)
#[derive(Debug, Clone)]
pub struct StampedValue {
    pub value: Expr,
    pub basis: crate::runtime::message::BasisStamp,
}

pub struct DefActor {
    pub name: String,
    pub value: Expr, // expr of def
    // pub is_assert_actor_of: Option<(TestId, ActorRef<Manager>)>,

    pub pubsub: PubSub,
    // pub lock_state: LockState,

    // read request is for transactions reading this def
    pub read_requests: HashMap<TxnId, (ActorRef<Manager>, Vec<TxnId>)>,
    // test read request is for assertion def actor, one shot request
    pub test_read_request: Option<(TestId, (ActorRef<Manager>, Vec<TxnId>))>,

    pub state: ChangeState,
    pub is_glitch_free: bool,
    pub buffered_outputs: HashMap<TxnId, (Expr, HashSet<Txn>)>,
    pub manager_addr: ActorRef<Manager>,
    
    // NEW: Buffering for basis checking (Phase 2)
    pub input_buffers: HashMap<String, Vec<StampedValue>>,
    pub current_inputs: HashMap<String, StampedValue>,
    pub current_basis: crate::runtime::message::BasisStamp,
}

impl DefActor {
    pub fn new(
        name: String,
        expr: Expr,                                    // def's expr
        value: Expr,                                   // def's initialized value
        arg_to_values: HashMap<String, Expr>,          // def's args to their initialized values
        arg_to_vars: HashMap<String, HashSet<String>>, // args to their transitively dependent vars
        // if arg itself is var, then arg_to_vars[arg] = {arg}
        is_glitch_free: bool,
        manager_addr: ActorRef<Manager>,
    ) -> DefActor {
        DefActor {
            name,
            value,
            // is_assert_actor_of: testid_and_manager,
            pubsub: PubSub::new(),
            // lock_state: LockState::new(),
            read_requests: HashMap::new(),
            test_read_request: None,
            state: ChangeState::new(expr, arg_to_values, arg_to_vars),
            is_glitch_free,
            buffered_outputs: HashMap::new(),
            manager_addr,
            
            // NEW: Initialize buffering structures
            input_buffers: HashMap::new(),
            current_inputs: HashMap::new(),
            current_basis: crate::runtime::message::BasisStamp::empty(),
        }
    }
    
    // ========================================================================
    // NEW: Basis Checking Methods (Phase 2)
    // ========================================================================
    
    /// Extract input variable names from the def's expression
    /// This tells us which variables this def depends on
    pub fn get_input_names(&self) -> Vec<String> {
        // Use the state's arg_to_values to get input names
        self.state.arg_to_values.keys().cloned().collect()
    }
    
    /// Try to compute the next value if we have a consistent batch of inputs
    /// Returns true if a value was computed and published
    pub async fn try_compute_next_value(&mut self) -> bool {
        let input_names = self.get_input_names();
        
        // Check if we have any buffered updates
        let has_updates = input_names.iter()
            .any(|name| self.input_buffers.get(name)
                .map(|buf| !buf.is_empty())
                .unwrap_or(false));
        
        if !has_updates {
            return false; // Nothing to do
        }
        
        println!("[DefActor {}] Trying to find consistent batch...", self.name);
        
        // Try to find a consistent batch
        if let Some(consistent_inputs) = self.find_consistent_batch(&input_names) {
            println!("[DefActor {}] Found consistent batch! Computing new value...", self.name);
            
            // Compute new value with these inputs
            if let Some(new_value) = self.evaluate_with_inputs(&consistent_inputs) {
                // Merge all input bases to get output basis
                let mut output_basis = crate::runtime::message::BasisStamp::empty();
                for stamped_value in consistent_inputs.values() {
                    output_basis.merge_from(&stamped_value.basis);
                }
                
                println!("[DefActor {}] New value computed: {:?}, basis: {:?}", 
                         self.name, new_value, output_basis);
                
                // Update current state
                self.value = new_value.clone();
                self.current_basis = output_basis.clone();
                self.current_inputs = consistent_inputs;
                
                // Publish the new value
                let msg = crate::runtime::message::Msg::PropChange {
                    from_name: self.name.clone(),
                    val: new_value,
                    preds: HashSet::new(), // TODO: compute proper preds
                    basis: output_basis,
                };
                
                self.pubsub.publish(msg).await;
                
                return true;
            }
        } else {
            println!("[DefActor {}] No consistent batch found yet, waiting for more updates", self.name);
        }
        
        false
    }
    
    /// Find a consistent batch of inputs where all dependencies are satisfied
    /// This is the core of the basis checking algorithm
    fn find_consistent_batch(
        &self,
        input_names: &[String],
    ) -> Option<HashMap<String, StampedValue>> {
        let mut result = HashMap::new();
        
        // For each input, try to get a value
        for input_name in input_names {
            // First check if we have a current value
            if let Some(current) = self.current_inputs.get(input_name) {
                result.insert(input_name.clone(), current.clone());
            }
            // Then check buffered updates
            else if let Some(buffer) = self.input_buffers.get(input_name) {
                if let Some(first_update) = buffer.first() {
                    result.insert(input_name.clone(), first_update.clone());
                } else {
                    println!("[DefActor {}] No value available for input '{}'", self.name, input_name);
                    return None; // No value available for this input
                }
            } else {
                println!("[DefActor {}] No buffer for input '{}'", self.name, input_name);
                return None; // No value available for this input
            }
        }
        
        // Verify we have all required inputs
        if result.len() != input_names.len() {
            return None;
        }
        
        println!("[DefActor {}] Consistent batch found with {} inputs", self.name, result.len());
        Some(result)
    }
    
    /// Evaluate expression with given input values
    fn evaluate_with_inputs(
        &self,
        inputs: &HashMap<String, StampedValue>,
    ) -> Option<Expr> {
        // Create a substitution map
        let mut subst = HashMap::new();
        for (name, stamped) in inputs {
            subst.insert(name.clone(), stamped.value.clone());
        }
        
        // Use the state's evaluation logic
        // For now, we'll use a simple substitution approach
        Some(self.substitute_expr(&self.state.expr, &subst))
    }
    
    /// Substitute variables in an expression with their values
    fn substitute_expr(&self, expr: &Expr, subst: &HashMap<String, Expr>) -> Expr {
        match expr {
            Expr::Variable { ident } => {
                subst.get(ident).cloned().unwrap_or_else(|| expr.clone())
            }
            Expr::Binop { op, expr1, expr2 } => {
                let left_val = self.substitute_expr(expr1, subst);
                let right_val = self.substitute_expr(expr2, subst);
                
                // Try to evaluate if both are constants
                self.eval_binop(op, &left_val, &right_val)
            }
            Expr::Unop { op, expr: operand } => {
                let operand_val = self.substitute_expr(operand, subst);
                self.eval_unop(op, &operand_val)
            }
            _ => expr.clone(),
        }
    }
    
    /// Evaluate a binary operation
    fn eval_binop(&self, op: &crate::ast::BinOp, left: &Expr, right: &Expr) -> Expr {
        use crate::ast::BinOp::*;
        use Expr::*;
        
        match (left, right) {
            (Number { val: l }, Number { val: r }) => {
                match op {
                    Add => Number { val: l + r },
                    Sub => Number { val: l - r },
                    Mul => Number { val: l * r },
                    Div if *r != 0 => Number { val: l / r },
                    Eq => Bool { val: l == r },
                    Lt => Bool { val: l < r },
                    Gt => Bool { val: l > r },
                    _ => Binop { 
                        op: op.clone(), 
                        expr1: Box::new(left.clone()), 
                        expr2: Box::new(right.clone()) 
                    },
                }
            }
            (Bool { val: l }, Bool { val: r }) => {
                match op {
                    And => Bool { val: *l && *r },
                    Or => Bool { val: *l || *r },
                    Eq => Bool { val: l == r },
                    _ => Binop { 
                        op: op.clone(), 
                        expr1: Box::new(left.clone()), 
                        expr2: Box::new(right.clone()) 
                    },
                }
            }
            _ => Binop { 
                op: op.clone(), 
                expr1: Box::new(left.clone()), 
                expr2: Box::new(right.clone()) 
            },
        }
    }
    
    /// Evaluate a unary operation
    fn eval_unop(&self, op: &crate::ast::UnOp, operand: &Expr) -> Expr {
        use crate::ast::UnOp::*;
        use Expr::*;
        
        match operand {
            Number { val } => {
                match op {
                    Neg => Number { val: -val },
                    _ => Unop { op: op.clone(), expr: Box::new(operand.clone()) },
                }
            }
            Bool { val } => {
                match op {
                    Not => Bool { val: !val },
                    _ => Unop { op: op.clone(), expr: Box::new(operand.clone()) },
                }
            }
            _ => Unop { op: op.clone(), expr: Box::new(operand.clone()) },
        }
    }
}
