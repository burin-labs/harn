use std::cell::RefCell;
use std::collections::BTreeSet;

use serde_json::{json, Value as JsonValue};

const OPERATOR_GRANT_SCHEMA: &str = "harn.operator-approval-grant.v1";

thread_local! {
    static OPERATOR_APPROVAL_GRANT_STACK: RefCell<Vec<OperatorApprovalGrant>> =
        const { RefCell::new(Vec::new()) };
}

/// Explicit, operator-supplied authority to run selected risky operations.
///
/// This type is separate from [`super::ToolApprovalPolicy`]. A pipeline can
/// scope ordinary approval policy, but it cannot construct this grant. The CLI
/// establishes it only from an explicit operator flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorApprovalGrant {
    operations: BTreeSet<String>,
}

impl OperatorApprovalGrant {
    /// Build a grant from exact operation names supplied by an operator.
    pub fn from_cli_operations(
        operations: impl IntoIterator<Item = String>,
    ) -> Result<Option<Self>, String> {
        let mut approved = BTreeSet::new();
        for raw in operations {
            let operation = raw.trim();
            if operation.is_empty() {
                return Err("--approve-risky requires a non-empty operation name".to_string());
            }
            if !is_operation_name(operation) {
                return Err(format!(
                    "--approve-risky accepts exact operation names such as `git.push`, not `{operation}`"
                ));
            }
            approved.insert(operation.to_string());
        }
        if approved.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            operations: approved,
        }))
    }

    /// Return an auditable receipt when this grant covers `operation`.
    pub fn receipt_for(&self, operation: &str) -> Option<JsonValue> {
        self.operations.contains(operation).then(|| {
            json!({
                "schema": OPERATOR_GRANT_SCHEMA,
                "approver": "cli",
                "grant_source": "explicit_cli_flag",
                "operation": operation,
                "approved": true,
            })
        })
    }
}

/// Restores the preceding CLI grant when the current invocation ends.
pub struct OperatorApprovalGrantGuard;

pub fn install_operator_approval_grant(grant: OperatorApprovalGrant) -> OperatorApprovalGrantGuard {
    OPERATOR_APPROVAL_GRANT_STACK.with(|stack| stack.borrow_mut().push(grant));
    OperatorApprovalGrantGuard
}

impl Drop for OperatorApprovalGrantGuard {
    fn drop(&mut self) {
        OPERATOR_APPROVAL_GRANT_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

pub fn current_operator_approval_grant() -> Option<OperatorApprovalGrant> {
    OPERATOR_APPROVAL_GRANT_STACK.with(|stack| stack.borrow().last().cloned())
}

pub(crate) fn clear_operator_approval_grants() {
    OPERATOR_APPROVAL_GRANT_STACK.with(|stack| stack.borrow_mut().clear());
}

pub(crate) fn swap_operator_approval_grant_stack(
    next: Vec<OperatorApprovalGrant>,
) -> Vec<OperatorApprovalGrant> {
    OPERATOR_APPROVAL_GRANT_STACK.with(|stack| std::mem::replace(&mut *stack.borrow_mut(), next))
}

fn is_operation_name(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some(character) if character.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_requires_exact_non_empty_operation_names() {
        assert!(OperatorApprovalGrant::from_cli_operations(Vec::new())
            .unwrap()
            .is_none());
        assert!(OperatorApprovalGrant::from_cli_operations(vec!["*".to_string()]).is_err());
        assert!(OperatorApprovalGrant::from_cli_operations(vec![".git.push".to_string()]).is_err());
        assert!(OperatorApprovalGrant::from_cli_operations(vec!["git push".to_string()]).is_err());

        let grant = OperatorApprovalGrant::from_cli_operations(vec!["git.push".to_string()])
            .unwrap()
            .expect("grant");
        assert!(grant.receipt_for("git.fetch").is_none());
        assert_eq!(grant.receipt_for("git.push").unwrap()["approver"], "cli");
    }
}
