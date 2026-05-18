use crate::error::SindriError;
use nickel_lang::{Context, Error, ErrorFormat, Expr};
use std::path::Path;

pub fn evaluate_file_with_contract(path: &Path, contract: &str, minimal_example: &str) -> Result<Expr, SindriError> {
    let display: String = path.display().to_string();
    let source: String = std::fs::read_to_string(path).map_err(|source| SindriError::Io {
        path: display.clone(),
        source,
    })?;
    if source.trim().is_empty() {
        return Err(SindriError::Schema {
            path: display,
            message: format!(
                "file is empty\n\nMinimal valid example:\n{}\n\nFull contract:\n{}",
                minimal_example.trim(),
                contract.trim(),
            ),
        });
    }
    // Evaluate the source on its own first so that parse/syntax errors reference
    // the actual file content, not the contract wrapper expression.
    let mut pre_check: Context = Context::new().with_source_name(display.clone());
    pre_check
        .eval_deep(&source)
        .map_err(|error| nickel_error(error, display.clone()))?;
    // Source is valid; now re-evaluate with the contract applied.
    let combined: String = format!("({source}) | ({contract})");
    let mut context: Context = Context::new().with_source_name(display.clone());
    context.eval_deep(&combined).map_err(|error| {
        let nickel_message: String = format!(
            "{}Minimal valid example:\n{}\n\nFull contract:\n{}",
            format_nickel_error(error),
            minimal_example.trim(),
            contract.trim(),
        );
        SindriError::NickelEval {
            path: display,
            nickel_message,
        }
    })
}

pub fn evaluate_file(path: &Path) -> Result<Expr, SindriError> {
    let display: String = path.display().to_string();
    let source: String = std::fs::read_to_string(path).map_err(|source| SindriError::Io {
        path: display.clone(),
        source,
    })?;
    let mut context: Context = Context::new().with_source_name(display.clone());
    context.eval_deep(&source).map_err(|error| nickel_error(error, display))
}

fn nickel_error(error: Error, path: String) -> SindriError {
    SindriError::NickelEval {
        path,
        nickel_message: format_nickel_error(error),
    }
}

fn format_nickel_error(error: Error) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    let _: Result<(), Box<dyn std::error::Error>> = error.format(&mut buffer, ErrorFormat::Text);
    String::from_utf8_lossy(&buffer).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn evaluate_file_missing_path() {
        let result: Result<Expr, SindriError> = evaluate_file(Path::new("/nonexistent/path/file.ncl"));
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }
}
