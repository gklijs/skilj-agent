//! skilj-codegen's `*.skilj.toml`: event and command shapes declared as
//! data, turned into Rust by the project's build.rs. Only the names and
//! the bounded context matter to the index; `decide()` bodies stay in
//! Rust as `decide_<snake_case name>` functions.

use crate::TypeKind;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct Declaration {
    pub bounded_context: String,
    #[serde(default)]
    event_type: Vec<Named>,
    #[serde(default)]
    command_type: Vec<Named>,
}

#[derive(Debug, Deserialize)]
struct Named {
    name: String,
}

impl Declaration {
    pub fn types(&self) -> impl Iterator<Item = (TypeKind, String)> + '_ {
        self.event_type
            .iter()
            .map(|e| (TypeKind::Event, e.name.clone()))
            .chain(self.command_type.iter().map(|c| (TypeKind::Command, c.name.clone())))
    }
}

pub(crate) fn parse(source: &str) -> anyhow::Result<Declaration> {
    Ok(toml::from_str(source)?)
}

/// `WithdrawMoney` -> `decide_withdraw_money`.
pub fn decide_fn_name(command: &str) -> String {
    let mut out = String::from("decide");
    for c in command.chars() {
        if c.is_uppercase() {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_fn_name_is_snake_case() {
        assert_eq!(decide_fn_name("WithdrawMoney"), "decide_withdraw_money");
        assert_eq!(decide_fn_name("Deposit"), "decide_deposit");
    }

    #[test]
    fn reads_names_and_context() {
        let decl = parse(
            r#"
            bounded_context = "banking"
            [[event_type]]
            name = "MoneyDeposited"
            fields = [{ name = "amount", type = "i64" }]
            [[command_type]]
            name = "DepositMoney"
            "#,
        )
        .unwrap();
        assert_eq!(decl.bounded_context, "banking");
        let types: Vec<_> = decl.types().collect();
        assert_eq!(
            types,
            vec![
                (TypeKind::Event, "MoneyDeposited".to_string()),
                (TypeKind::Command, "DepositMoney".to_string())
            ]
        );
    }
}
