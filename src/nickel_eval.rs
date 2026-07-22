use crate::error::SindriError;
use crate::error::SindriResult;
use crate::runtime::FileSystem;
use crate::types::ConfigFile;
use nickel_lang::Context;
use nickel_lang::Error as NickelError;
use nickel_lang::ErrorFormat;
use nickel_lang::Expr;
use std::io::Error as IoError;
use std::path::PathBuf;

/// A Nickel contract together with the minimal valid example shown to the user when a config file
/// fails to satisfy it. The two are always authored and surfaced as a pair, so they travel as one.
pub struct Contract {
    definition: &'static str,
    minimal_example: &'static str,
}

impl Contract {
    pub const fn new(definition: &'static str, minimal_example: &'static str) -> Contract {
        Contract {
            definition,
            minimal_example,
        }
    }

    pub fn definition(&self) -> &str {
        self.definition
    }

    pub fn minimal_example(&self) -> &str {
        self.minimal_example
    }
}

pub struct Nickel;

impl Nickel {
    pub fn evaluate_with_contract(
        config_file: &ConfigFile,
        contract: &Contract,
        file_system: &impl FileSystem,
    ) -> SindriResult<Expr> {
        let display: String = config_file.workspace_path().to_string();
        let source: String =
            file_system
                .read_to_string(config_file.disk_path().as_ref())
                .map_err(|source: IoError| -> SindriError {
                    SindriError::Io {
                        path: config_file.workspace_path().as_ref().to_path_buf(),
                        source,
                    }
                })?;
        if source.trim().is_empty() {
            return Err(SindriError::Schema {
                path: config_file.workspace_path().as_ref().to_path_buf(),
                message: format!(
                    "file is empty\n\nMinimal valid example:\n{}\n\nFull contract:\n{}",
                    contract.minimal_example().trim(),
                    contract.definition().trim(),
                ),
            });
        }
        // Evaluate the source on its own first so that parse/syntax errors reference
        // the actual file content, not the contract wrapper expression.
        let mut pre_check: Context = Context::new().with_source_name(display.clone());
        pre_check
            .eval_deep(&source)
            .map_err(|error: NickelError| -> SindriError {
                nickel_error(error, config_file.workspace_path().as_ref().to_path_buf())
            })?;
        // Source is valid; now re-evaluate with the contract applied.
        let combined: String = format!("({source}) | ({})", contract.definition());
        let mut context: Context = Context::new().with_source_name(display);
        context.eval_deep(&combined).map_err(|error| {
            let nickel_message: String = format!(
                "{}Minimal valid example:\n{}\n\nFull contract:\n{}",
                format_nickel_error(error),
                contract.minimal_example().trim(),
                contract.definition().trim(),
            );
            SindriError::NickelEval {
                path: config_file.workspace_path().as_ref().to_path_buf(),
                nickel_message,
            }
        })
    }

    /// Evaluate a self-contained Nickel source string (no file imports) deeply, labelling any error's
    /// source spans with `source_name`. Used for task scripts, whose source Sindri assembles in memory
    /// rather than reads from a config file; the caller wraps the returned message in its own error.
    pub fn evaluate_source(source: &str, source_name: &str) -> Result<Expr, String> {
        let mut context: Context = Context::new().with_source_name(source_name.to_string());
        context.eval_deep(source).map_err(format_nickel_error)
    }

    /// Render `value` as a Nickel string literal, escaping the characters that would otherwise end
    /// the string or be read as an escape. Used to embed Sindri-supplied strings — paths, field
    /// names — into Nickel source Sindri assembles in memory.
    pub fn string_literal(value: &str) -> String {
        let mut escaped: String = String::with_capacity(value.len() + 2);
        escaped.push('"');
        for character in value.chars() {
            match character {
                '"' => escaped.push_str("\\\""),
                '\\' => escaped.push_str("\\\\"),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                other => escaped.push(other),
            }
        }
        escaped.push('"');
        escaped
    }

    pub fn evaluate(config_file: &ConfigFile, file_system: &impl FileSystem) -> SindriResult<Expr> {
        let display: String = config_file.workspace_path().to_string();
        let source: String =
            file_system
                .read_to_string(config_file.disk_path().as_ref())
                .map_err(|source: IoError| -> SindriError {
                    SindriError::Io {
                        path: config_file.workspace_path().as_ref().to_path_buf(),
                        source,
                    }
                })?;
        let mut context: Context = Context::new().with_source_name(display);
        context.eval_deep(&source).map_err(|error: NickelError| -> SindriError {
            nickel_error(error, config_file.workspace_path().as_ref().to_path_buf())
        })
    }
}

fn nickel_error(error: NickelError, path: PathBuf) -> SindriError {
    SindriError::NickelEval {
        path,
        nickel_message: format_nickel_error(error),
    }
}

fn format_nickel_error(error: NickelError) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    let _: Result<(), Box<dyn std::error::Error>> = error.format(&mut buffer, ErrorFormat::Text);
    String::from_utf8_lossy(&buffer).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DummyRuntime;
    use crate::types::AbsoluteFile;
    use crate::types::RelativeFile;
    use nickel_lang::Record;
    use std::path::PathBuf;

    const CONTRACT: Contract = Contract::new("{ name | String }", r#"{ name = "demo" }"#);

    fn config_file() -> ConfigFile {
        ConfigFile::new(
            AbsoluteFile::new(PathBuf::from("/file.ncl")),
            RelativeFile::new_unchecked("file.ncl"),
        )
    }

    #[test]
    fn contract_exposes_its_definition_and_minimal_example() {
        assert_eq!(CONTRACT.definition(), "{ name | String }");
        assert_eq!(CONTRACT.minimal_example(), r#"{ name = "demo" }"#);
    }

    #[test]
    fn evaluate_with_contract_missing_path() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let result: SindriResult<Expr> = Nickel::evaluate_with_contract(&config_file(), &CONTRACT, &runtime);
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn evaluate_with_contract_empty_file() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/file.ncl", "  \n  ").build();
        let result: SindriResult<Expr> = Nickel::evaluate_with_contract(&config_file(), &CONTRACT, &runtime);
        assert!(matches!(result, Err(SindriError::Schema { .. })));
    }

    #[test]
    fn evaluate_with_contract_syntax_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/file.ncl", "{ name = }").build();
        let result: SindriResult<Expr> = Nickel::evaluate_with_contract(&config_file(), &CONTRACT, &runtime);
        assert!(matches!(result, Err(SindriError::NickelEval { .. })));
    }

    #[test]
    fn evaluate_with_contract_passes_a_conforming_source() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/file.ncl", r#"{ name = "demo" }"#)
            .build();
        let expression: Expr = Nickel::evaluate_with_contract(&config_file(), &CONTRACT, &runtime).unwrap();
        let record: Record = expression.as_record().expect("expected a record");
        let name: Expr = record.value_by_name("name").expect("missing field 'name'");
        assert_eq!(name.as_str().expect("expected a string"), "demo");
    }

    #[test]
    fn string_literal_escapes_characters_that_would_end_or_be_read_as_an_escape() {
        assert_eq!(Nickel::string_literal("plain"), "\"plain\"");
        assert_eq!(
            Nickel::string_literal("a\"b\\c\nd\re\tf"),
            "\"a\\\"b\\\\c\\nd\\re\\tf\""
        );
    }

    #[test]
    fn evaluate_missing_path() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let result: SindriResult<Expr> = Nickel::evaluate(&config_file(), &runtime);
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn evaluate_reads_and_parses_registered_source() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/file.ncl", r#"{ name = "demo" }"#)
            .build();
        let expression: Expr = Nickel::evaluate(&config_file(), &runtime).unwrap();
        let record: Record = expression.as_record().expect("expected a record");
        let name: Expr = record.value_by_name("name").expect("missing field 'name'");
        assert_eq!(name.as_str().expect("expected a string"), "demo");
    }
}
