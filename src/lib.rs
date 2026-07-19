pub mod cli;
pub mod error;
pub mod executor;
pub mod file_set;
pub mod glob;
pub mod go_work;
pub mod hash;
pub mod lifecycle;
pub mod local_time_with_elapsed;
pub mod module;
pub mod module_graph;
pub mod nickel_eval;
pub mod parameter;
pub mod plugin;
pub mod runtime;
pub mod script;
pub mod state;
pub mod telemetry;
pub mod types;
pub mod workspace;

#[cfg(test)]
mod tests {
    use nickel_lang::{Context, Expr, Record};

    #[test]
    fn nickel_evaluates_record() {
        let mut context: Context = Context::new();
        let expression: Expr = context
            .eval_deep(r#"{ name = "sindri", version = "0.1.0" }"#)
            .expect("evaluation failed");
        let record: Record = expression.as_record().expect("expected a record");
        let name_value: Expr = record.value_by_name("name").expect("missing field 'name'");
        let name: &str = name_value.as_str().expect("expected a string");
        assert_eq!(name, "sindri");
    }
}
