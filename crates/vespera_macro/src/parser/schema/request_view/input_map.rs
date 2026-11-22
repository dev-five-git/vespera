//! The computed input-name map of `request_view`: component names and
//! Rust-ident aliases → input component names, and the reverse owner map. A
//! sidecar of `request_view`.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
};

/// The input components of the documented types.
///
/// `names` maps a component name (and a Rust-ident alias of a renamed schema)
/// to its input component name, for every type whose request view differs.
/// `owners` is the reverse map, input component name → the component it views:
/// only a component owns its input, never an alias of another type that
/// happens to sort first.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InputMap {
    pub(super) names: BTreeMap<String, String>,
    pub(super) owners: BTreeMap<String, String>,
    /// Every registry key of a documented type (component names, Rust idents,
    /// scoped keys), whether or not its request view differs. A registered key
    /// absent from `names` names a type with no input view, so the bare-ident
    /// fallback must not decide for it.
    pub(super) registered: BTreeSet<String>,
}

impl Deref for InputMap {
    type Target = BTreeMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.names
    }
}

impl InputMap {
    /// This map without the components whose input `drop` names (owners
    /// included, so the remaining inputs keep their components).
    pub fn without_inputs(&self, drop: impl Fn(&String) -> bool) -> Self {
        Self {
            names: self
                .names
                .iter()
                .filter(|(_, input)| !drop(input))
                .map(|(component, input)| (component.clone(), input.clone()))
                .collect(),
            owners: self
                .owners
                .iter()
                .filter(|(input, _)| !drop(input))
                .map(|(input, component)| (input.clone(), component.clone()))
                .collect(),
            registered: self.registered.clone(),
        }
    }
}

impl From<BTreeMap<String, String>> for InputMap {
    /// Without a computed owner map, an input is owned by its smallest name.
    fn from(names: BTreeMap<String, String>) -> Self {
        let mut owners = BTreeMap::new();
        for (name, input) in &names {
            owners.entry(input.clone()).or_insert_with(|| name.clone());
        }
        Self {
            names,
            owners,
            registered: BTreeSet::new(),
        }
    }
}
