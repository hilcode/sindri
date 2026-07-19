use crate::error::SindriError;
use crate::error::SindriResult;
use crate::nickel_eval::Nickel;
use blake3::Hasher;
use smol_str::SmolStr;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;

/// A field delimiter folded into the binding hash between values, so that two different splittings
/// of the same byte stream can never collide.
const FIELD_SEPARATOR: [u8; 1] = [0];

/// The plugin that owns a [`Parameter`], namespacing its name so two plugins may each define a
/// parameter of the same name without collision.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PluginName(SmolStr);

impl PluginName {
    pub fn new(name: impl Into<SmolStr>) -> PluginName {
        PluginName(name.into())
    }
}

impl Display for PluginName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

/// The name of a [`Parameter`], unique within its owning plugin rather than globally.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ParameterName(SmolStr);

impl ParameterName {
    pub fn new(name: impl Into<SmolStr>) -> ParameterName {
        ParameterName(name.into())
    }
}

impl Display for ParameterName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

/// The Nickel contract source constraining a [`Parameter`]'s legal values (`"Bool"`,
/// `"std.contract.from_predicate (fun value => value == \"debug\" || value == \"release\")"`, …),
/// authored inline by the plugin that owns the parameter rather than drawn from a shared type
/// registry.
#[derive(Clone, Debug)]
pub struct ParameterType(SmolStr);

impl ParameterType {
    pub fn new(contract_source: impl Into<SmolStr>) -> ParameterType {
        ParameterType(contract_source.into())
    }

    pub fn source(&self) -> &str {
        &self.0
    }
}

/// A named build setting owned by a plugin, identified by `(plugin, name)` and constrained by an
/// inline [`ParameterType`].
#[derive(Clone, Debug)]
pub struct Parameter {
    plugin: PluginName,
    name: ParameterName,
    parameter_type: ParameterType,
}

impl Parameter {
    pub fn new(plugin: PluginName, name: ParameterName, parameter_type: ParameterType) -> Parameter {
        Parameter {
            plugin,
            name,
            parameter_type,
        }
    }

    pub fn plugin(&self) -> &PluginName {
        &self.plugin
    }

    pub fn name(&self) -> &ParameterName {
        &self.name
    }

    pub fn parameter_type(&self) -> &ParameterType {
        &self.parameter_type
    }
}

/// A task's declared parameters: authored data naming every [`Parameter`] its `Script` requires,
/// read without evaluating the script itself. [`ParameterBinding::resolve`] checks a binding against
/// this set before the script ever runs. Kept unique and ordered by `(plugin, name)`, so validation
/// and hashing are stable regardless of the order the parameters were declared in.
#[derive(Clone, Debug, Default)]
pub struct ParameterDeclarations {
    parameters: BTreeMap<(PluginName, ParameterName), ParameterType>,
}

impl ParameterDeclarations {
    pub fn new(parameters: impl IntoIterator<Item = Parameter>) -> ParameterDeclarations {
        let parameters: BTreeMap<(PluginName, ParameterName), ParameterType> = parameters
            .into_iter()
            .map(|parameter: Parameter| -> ((PluginName, ParameterName), ParameterType) {
                ((parameter.plugin, parameter.name), parameter.parameter_type)
            })
            .collect();
        ParameterDeclarations { parameters }
    }

    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }

    /// The declared parameters in `(plugin, name)` order, so a definition hash folded over this
    /// iterator is stable regardless of the order the parameters were declared in.
    pub fn iter(&self) -> impl Iterator<Item = (&PluginName, &ParameterName, &ParameterType)> {
        self.parameters
            .iter()
            .map(|((plugin, name), parameter_type)| (plugin, name, parameter_type))
    }
}

/// A single parameter's bound value, held as Nickel expression source. It is embedded directly into
/// the record a `Script` reads its parameters from, and checked against its [`ParameterType`] before
/// that happens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterValue(SmolStr);

impl ParameterValue {
    pub fn new(source: impl Into<SmolStr>) -> ParameterValue {
        ParameterValue(source.into())
    }

    pub fn source(&self) -> &str {
        &self.0
    }
}

/// Candidate parameter values supplied by module or build configuration, keyed by the `(plugin,
/// name)` identity of the [`Parameter`] each is meant for. A task's declared parameters may not all
/// have a value here, and this may carry values for parameters no declared set names —
/// [`ParameterBinding::resolve`] is what checks both against a [`ParameterDeclarations`].
#[derive(Clone, Debug, Default)]
pub struct ParameterValues {
    values: BTreeMap<(PluginName, ParameterName), ParameterValue>,
}

impl ParameterValues {
    pub fn new(values: impl IntoIterator<Item = (PluginName, ParameterName, ParameterValue)>) -> ParameterValues {
        ParameterValues {
            values: values
                .into_iter()
                .map(|(plugin, name, value): (PluginName, ParameterName, ParameterValue)| -> ((PluginName, ParameterName), ParameterValue) {
                    ((plugin, name), value)
                })
                .collect(),
        }
    }

    fn get(&self, plugin: &PluginName, name: &ParameterName) -> Option<&ParameterValue> {
        self.values.get(&(plugin.clone(), name.clone()))
    }
}

/// A blake3 digest of a [`ParameterBinding`]'s resolved values, ordered by `(plugin, name)`.
/// Distinct from the other hash newtypes so a binding digest can never be compared against a
/// pattern or content digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingHash([u8; 32]);

/// The resolved values for every parameter a task's `Script` requires, checked against its
/// [`ParameterDeclarations`] before the script is ever evaluated: [`ParameterBinding::resolve`]
/// guarantees a value exists for each declared parameter and that the value satisfies the
/// parameter's own [`ParameterType`].
#[derive(Clone, Debug)]
pub struct ParameterBinding {
    values: BTreeMap<(PluginName, ParameterName), ParameterValue>,
}

impl ParameterBinding {
    /// Bind `values` against `declared`, in `(plugin, name)` order: every declared parameter must
    /// have a value, and that value must satisfy the parameter's own `ParameterType` contract. Both
    /// checks run here, before a `Script` is ever evaluated, so a failure names the offending
    /// parameter directly rather than surfacing as a Nickel error deep inside the script.
    pub fn resolve(declared: &ParameterDeclarations, values: &ParameterValues) -> SindriResult<ParameterBinding> {
        let mut bound: BTreeMap<(PluginName, ParameterName), ParameterValue> = BTreeMap::new();
        for ((plugin, name), parameter_type) in &declared.parameters {
            let value: &ParameterValue = values.get(plugin, name).ok_or_else(|| -> SindriError {
                SindriError::ParameterMissing {
                    plugin: plugin.clone(),
                    parameter: name.clone(),
                }
            })?;
            let combined: String = format!("({}) | ({})", value.source(), parameter_type.source());
            let source_name: String = format!("parameter `{plugin}.{name}`");
            Nickel::evaluate_source(&combined, &source_name).map_err(|nickel_message: String| -> SindriError {
                SindriError::ParameterInvalid {
                    plugin: plugin.clone(),
                    parameter: name.clone(),
                    nickel_message,
                }
            })?;
            bound.insert((plugin.clone(), name.clone()), value.clone());
        }
        Ok(ParameterBinding { values: bound })
    }

    /// The binding of a task whose script requires no parameters.
    pub fn empty() -> ParameterBinding {
        ParameterBinding {
            values: BTreeMap::new(),
        }
    }

    /// A digest over the resolved values, in `(plugin, name)` order, so the same bindings always
    /// reproduce the same hash regardless of the order `values` were supplied in.
    pub fn binding_hash(&self) -> BindingHash {
        let mut hasher: Hasher = Hasher::new();
        for value in self.values.values() {
            hasher.update(value.source().as_bytes());
            hasher.update(&FIELD_SEPARATOR);
        }
        BindingHash(*hasher.finalize().as_bytes())
    }

    /// The Nickel record literal for the `params` value a `Script` is applied to: one nested record
    /// per plugin, itself one field per that plugin's bound parameter — a script reads its own values
    /// as `params."<plugin>".<name>`. Nesting by plugin, rather than naming fields after the parameter
    /// alone, means two plugins can never collide on a same-named parameter: the `(PluginName,
    /// ParameterName)` key this binding is already stored under carries through to the rendered
    /// record instead of being dropped. Reading any other field is a plain "missing field" error — a
    /// record literal never carries fields beyond the ones it defines, so there is nothing further to
    /// close.
    pub fn to_nickel_record(&self) -> String {
        let mut by_plugin: BTreeMap<&PluginName, Vec<String>> = BTreeMap::new();
        for ((plugin, name), value) in &self.values {
            by_plugin
                .entry(plugin)
                .or_default()
                .push(format!("{} = {}", Nickel::string_literal(&name.to_string()), value.source()));
        }
        let plugins: Vec<String> = by_plugin
            .into_iter()
            .map(|(plugin, fields): (&PluginName, Vec<String>)| -> String {
                format!(
                    "{} = {{ {} }}",
                    Nickel::string_literal(&plugin.to_string()),
                    fields.join(", ")
                )
            })
            .collect();
        format!("{{ {} }}", plugins.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> PluginName {
        PluginName::new("plugin")
    }

    fn declared(parameter_type: &str) -> ParameterDeclarations {
        ParameterDeclarations::new([Parameter::new(
            plugin(),
            ParameterName::new("mode"),
            ParameterType::new(parameter_type),
        )])
    }

    fn values(value: &str) -> ParameterValues {
        ParameterValues::new([(plugin(), ParameterName::new("mode"), ParameterValue::new(value))])
    }

    #[test]
    fn a_parameter_exposes_its_plugin_name_and_type() {
        let parameter: Parameter = Parameter::new(plugin(), ParameterName::new("mode"), ParameterType::new("String"));
        assert_eq!(parameter.plugin(), &plugin());
        assert_eq!(parameter.name(), &ParameterName::new("mode"));
        assert_eq!(parameter.parameter_type().source(), "String");
    }

    #[test]
    fn declared_parameters_report_whether_they_are_empty() {
        assert!(ParameterDeclarations::default().is_empty());
        assert!(!declared("String").is_empty());
    }

    #[test]
    fn a_missing_value_errors_before_the_script_runs_naming_the_parameter() {
        let result: SindriResult<ParameterBinding> =
            ParameterBinding::resolve(&declared("String"), &ParameterValues::default());
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ParameterMissing { .. }));
        assert!(error.to_string().contains("mode"), "message was: {error}");
    }

    #[test]
    fn an_out_of_domain_value_is_a_contract_error() {
        let declared: ParameterDeclarations =
            declared(r#"std.contract.from_predicate (fun value => value == "debug" || value == "release")"#);
        let result: SindriResult<ParameterBinding> = ParameterBinding::resolve(&declared, &values(r#""turbo""#));
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ParameterInvalid { .. }));
    }

    #[test]
    fn declared_parameters_are_iterated_in_plugin_and_name_order() {
        let declared: ParameterDeclarations = ParameterDeclarations::new([
            Parameter::new(
                PluginName::new("z-plugin"),
                ParameterName::new("mode"),
                ParameterType::new("String"),
            ),
            Parameter::new(
                PluginName::new("a-plugin"),
                ParameterName::new("mode"),
                ParameterType::new("String"),
            ),
        ]);
        let plugins: Vec<PluginName> = declared.iter().map(|(plugin, _, _)| plugin.clone()).collect();
        assert_eq!(plugins, vec![PluginName::new("a-plugin"), PluginName::new("z-plugin")]);
    }

    #[test]
    fn a_well_typed_value_resolves() {
        let declared: ParameterDeclarations =
            declared(r#"std.contract.from_predicate (fun value => value == "debug" || value == "release")"#);
        let binding: ParameterBinding = ParameterBinding::resolve(&declared, &values(r#""debug""#)).unwrap();
        assert_eq!(binding.to_nickel_record(), r#"{ "plugin" = { "mode" = "debug" } }"#);
    }

    #[test]
    fn two_plugins_sharing_a_parameter_name_do_not_collide() {
        let declared: ParameterDeclarations = ParameterDeclarations::new([
            Parameter::new(PluginName::new("plugin-a"), ParameterName::new("mode"), ParameterType::new("String")),
            Parameter::new(PluginName::new("plugin-b"), ParameterName::new("mode"), ParameterType::new("String")),
        ]);
        let values: ParameterValues = ParameterValues::new([
            (PluginName::new("plugin-a"), ParameterName::new("mode"), ParameterValue::new(r#""debug""#)),
            (PluginName::new("plugin-b"), ParameterName::new("mode"), ParameterValue::new(r#""release""#)),
        ]);
        let binding: ParameterBinding = ParameterBinding::resolve(&declared, &values).unwrap();
        assert_eq!(
            binding.to_nickel_record(),
            r#"{ "plugin-a" = { "mode" = "debug" }, "plugin-b" = { "mode" = "release" } }"#
        );
    }

    #[test]
    fn different_values_produce_different_binding_hashes() {
        let declared: ParameterDeclarations = declared("String");
        let debug: ParameterBinding = ParameterBinding::resolve(&declared, &values(r#""debug""#)).unwrap();
        let release: ParameterBinding = ParameterBinding::resolve(&declared, &values(r#""release""#)).unwrap();
        assert_ne!(debug.binding_hash(), release.binding_hash());
    }

    #[test]
    fn the_same_values_reproduce_the_same_binding_hash() {
        let declared: ParameterDeclarations = declared("String");
        let first: ParameterBinding = ParameterBinding::resolve(&declared, &values(r#""debug""#)).unwrap();
        let second: ParameterBinding = ParameterBinding::resolve(&declared, &values(r#""debug""#)).unwrap();
        assert_eq!(first.binding_hash(), second.binding_hash());
    }

    #[test]
    fn an_empty_binding_has_a_stable_hash_and_an_empty_record() {
        let binding: ParameterBinding = ParameterBinding::empty();
        assert_eq!(binding.binding_hash(), ParameterBinding::empty().binding_hash());
        assert_eq!(binding.to_nickel_record(), "{  }");
    }
}
