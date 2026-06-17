//! RFC 8693 actor/principal chain support.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::value::{DictMap, VmValue};

const SUB: &str = "sub";
const ACT: &str = "act";
const MAY_ACT: &str = "may_act";
const SCOPES: &str = "scopes";
const SCOPE: &str = "scope";

/// A first-class RFC 8693 §4.1 actor chain.
///
/// `origin` serializes as the top-level `sub` claim: the human principal the
/// request is ultimately on behalf of. `actors` serializes as nested `act`
/// claims in RFC order, with the current actor outermost and prior actors
/// nested beneath it. For authorization, only the top-level token claims and
/// [`ActorChain::current`] are authoritative. The audit-not-authz rule is that
/// nested actors are audit history only and must not be used to grant access.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActorChain {
    origin: ActorChainEntry,
    actors: Vec<ActorChainEntry>,
    may_act: Option<String>,
}

/// Alias for APIs that name the full acting chain as a principal.
pub type Principal = ActorChain;

/// One subject in an [`ActorChain`], plus the authority scopes that subject
/// carried at that hop when the host knows them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActorChainEntry {
    subject: String,
    scopes: BTreeSet<String>,
}

impl ActorChainEntry {
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            scopes: BTreeSet::new(),
        }
    }

    pub fn with_scopes<I, S>(subject: impl Into<String>, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            subject: subject.into(),
            scopes: normalize_scopes(scopes),
        }
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn scopes(&self) -> impl Iterator<Item = &str> {
        self.scopes.iter().map(String::as_str)
    }

    pub fn scope_set(&self) -> &BTreeSet<String> {
        &self.scopes
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        let mut node = serde_json::Map::new();
        node.insert(
            SUB.to_string(),
            serde_json::Value::String(self.subject.clone()),
        );
        if !self.scopes.is_empty() {
            node.insert(
                SCOPES.to_string(),
                serde_json::Value::Array(
                    self.scopes
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
        serde_json::Value::Object(node)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeAttenuationMode {
    Off,
    #[default]
    NonIncreasing,
    StrictSubset,
}

impl ScopeAttenuationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::NonIncreasing => "non-increasing",
            Self::StrictSubset => "strict-subset",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeAttenuationPolicy {
    pub mode: ScopeAttenuationMode,
    pub alert_on_violation: bool,
}

impl Default for ScopeAttenuationPolicy {
    fn default() -> Self {
        Self {
            mode: ScopeAttenuationMode::NonIncreasing,
            alert_on_violation: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeAttenuationViolation {
    parent_subject: String,
    child_subject: String,
    parent_scopes: Vec<String>,
    child_scopes: Vec<String>,
    extra_scopes: Vec<String>,
    mode: ScopeAttenuationMode,
}

impl ScopeAttenuationViolation {
    fn new(parent: &ActorChainEntry, child: &ActorChainEntry, mode: ScopeAttenuationMode) -> Self {
        let extra_scopes = child
            .scopes
            .difference(&parent.scopes)
            .cloned()
            .collect::<Vec<_>>();
        Self {
            parent_subject: parent.subject.clone(),
            child_subject: child.subject.clone(),
            parent_scopes: parent.scopes.iter().cloned().collect(),
            child_scopes: child.scopes.iter().cloned().collect(),
            extra_scopes,
            mode,
        }
    }

    pub fn parent_subject(&self) -> &str {
        &self.parent_subject
    }

    pub fn child_subject(&self) -> &str {
        &self.child_subject
    }

    pub fn parent_scopes(&self) -> &[String] {
        &self.parent_scopes
    }

    pub fn child_scopes(&self) -> &[String] {
        &self.child_scopes
    }

    pub fn extra_scopes(&self) -> &[String] {
        &self.extra_scopes
    }

    pub fn mode(&self) -> ScopeAttenuationMode {
        self.mode
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "scope_attenuation_violation",
            "mode": self.mode.as_str(),
            "parent_subject": self.parent_subject,
            "child_subject": self.child_subject,
            "parent_scopes": self.parent_scopes,
            "child_scopes": self.child_scopes,
            "extra_scopes": self.extra_scopes,
        })
    }
}

impl fmt::Display for ScopeAttenuationViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.extra_scopes.is_empty() {
            write!(
                f,
                "scope attenuation violation: child `{}` must hold a strict subset of parent `{}` scopes",
                self.child_subject, self.parent_subject
            )
        } else {
            write!(
                f,
                "scope attenuation violation: child `{}` has scopes not held by parent `{}`: {}",
                self.child_subject,
                self.parent_subject,
                self.extra_scopes.join(", ")
            )
        }
    }
}

impl std::error::Error for ScopeAttenuationViolation {}

impl ActorChain {
    /// Create a chain with only an originating subject and no acting agents.
    pub fn new(origin: impl Into<String>) -> Self {
        Self {
            origin: ActorChainEntry::new(origin),
            actors: Vec::new(),
            may_act: None,
        }
    }

    pub fn new_with_scopes<I, S>(origin: impl Into<String>, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            origin: ActorChainEntry::with_scopes(origin, scopes),
            actors: Vec::new(),
            may_act: None,
        }
    }

    /// Build a chain from actors already ordered current-first, matching the
    /// RFC 8693 nested `act` traversal order.
    pub fn from_parts<I, S>(origin: impl Into<String>, actors: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            origin: ActorChainEntry::new(origin),
            actors: actors.into_iter().map(ActorChainEntry::new).collect(),
            may_act: None,
        }
    }

    pub fn from_entries<I>(origin: ActorChainEntry, actors: I) -> Self
    where
        I: IntoIterator<Item = ActorChainEntry>,
    {
        Self {
            origin,
            actors: actors.into_iter().collect(),
            may_act: None,
        }
    }

    /// Insert `actor` as the new current actor.
    pub fn push(&mut self, actor: impl Into<String>) {
        self.push_entry(ActorChainEntry::new(actor));
    }

    pub fn push_entry(&mut self, actor: ActorChainEntry) {
        self.actors.insert(0, actor);
    }

    /// Chainable form of [`ActorChain::push`].
    pub fn pushed(mut self, actor: impl Into<String>) -> Self {
        self.push(actor);
        self
    }

    pub fn pushed_with_scopes<I, S>(mut self, actor: impl Into<String>, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.push_entry(ActorChainEntry::with_scopes(actor, scopes));
        self
    }

    pub fn pushed_entry(mut self, actor: ActorChainEntry) -> Self {
        self.push_entry(actor);
        self
    }

    /// The actor that authorization checks should consider authoritative.
    ///
    /// When no delegation has occurred, the originating subject is also the
    /// current actor.
    pub fn current(&self) -> &str {
        self.actors
            .first()
            .map(ActorChainEntry::subject)
            .unwrap_or(self.origin.subject())
    }

    /// The originating human principal carried in the top-level `sub` claim.
    pub fn origin(&self) -> &str {
        self.origin.subject()
    }

    pub fn origin_entry(&self) -> &ActorChainEntry {
        &self.origin
    }

    pub fn current_entry(&self) -> &ActorChainEntry {
        self.actors.first().unwrap_or(&self.origin)
    }

    /// Acting agents in RFC nested-`act` order: current actor first.
    pub fn actors(&self) -> impl Iterator<Item = &str> {
        self.actors.iter().map(ActorChainEntry::subject)
    }

    pub fn actor_entries(&self) -> impl Iterator<Item = &ActorChainEntry> {
        self.actors.iter()
    }

    /// All principals from current actor through the originating subject.
    pub fn iter(&self) -> ActorChainIter<'_> {
        ActorChainIter {
            inner: self.entries(),
        }
    }

    pub fn entries(&self) -> ActorChainEntryIter<'_> {
        ActorChainEntryIter {
            actors: self.actors.iter(),
            origin: Some(&self.origin),
        }
    }

    pub fn parent_chain(&self) -> Option<Self> {
        if self.actors.is_empty() {
            return None;
        }
        Some(Self {
            origin: self.origin.clone(),
            actors: self.actors.iter().skip(1).cloned().collect(),
            may_act: self.may_act.clone(),
        })
    }

    pub fn is_delegated(&self) -> bool {
        !self.actors.is_empty()
    }

    /// Authorized actor hint from the optional RFC 8693 §4.4 `may_act` claim.
    pub fn may_act(&self) -> Option<&str> {
        self.may_act.as_deref()
    }

    pub fn set_may_act(&mut self, may_act: impl Into<String>) {
        self.may_act = Some(may_act.into());
    }

    pub fn with_may_act(mut self, may_act: impl Into<String>) -> Self {
        self.set_may_act(may_act);
        self
    }

    pub fn clear_may_act(&mut self) {
        self.may_act = None;
    }

    pub fn validate_scope_attenuation(
        &self,
        policy: &ScopeAttenuationPolicy,
    ) -> Result<(), ScopeAttenuationViolation> {
        match policy.mode {
            ScopeAttenuationMode::Off => return Ok(()),
            ScopeAttenuationMode::NonIncreasing | ScopeAttenuationMode::StrictSubset => {}
        }

        let root_to_current = self
            .entries()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        for pair in root_to_current.windows(2) {
            let parent = pair[0];
            let child = pair[1];
            let subset = child.scopes.is_subset(&parent.scopes);
            let strict = child.scopes.len() < parent.scopes.len();
            let valid = match policy.mode {
                ScopeAttenuationMode::Off => true,
                ScopeAttenuationMode::NonIncreasing => subset,
                ScopeAttenuationMode::StrictSubset => subset && strict,
            };
            if !valid {
                return Err(ScopeAttenuationViolation::new(parent, child, policy.mode));
            }
        }
        Ok(())
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        let mut root = principal_json_object(&self.origin);
        if let Some(act) = actor_nodes_to_json(&self.actors) {
            root.insert(ACT.to_string(), act);
        }
        if let Some(may_act) = &self.may_act {
            root.insert(MAY_ACT.to_string(), principal_claim_json(may_act));
        }
        serde_json::Value::Object(root)
    }

    pub fn from_json_value(value: &serde_json::Value) -> Result<Self, ActorChainError> {
        let root = expect_json_object(value, "$")?;
        let origin = json_entry(root, "$")?;
        let actors = json_actor_subjects(root.get(ACT), "$.act")?;
        let may_act = root
            .get(MAY_ACT)
            .map(|value| {
                let may_act = expect_json_object(value, "$.may_act")?;
                json_sub(may_act, "$.may_act")
            })
            .transpose()?;
        Ok(Self {
            origin,
            actors,
            may_act,
        })
    }

    pub fn to_vm_value(&self) -> VmValue {
        crate::schema::json_to_vm_value(&self.to_json_value())
    }

    pub fn from_vm_value(value: &VmValue) -> Result<Self, ActorChainError> {
        let root = expect_vm_dict(value, "$")?;
        let origin = vm_entry(root, "$")?;
        let actors = vm_actor_subjects(root.get(ACT), "$.act")?;
        let may_act = root
            .get(MAY_ACT)
            .map(|value| {
                let may_act = expect_vm_dict(value, "$.may_act")?;
                vm_sub(may_act, "$.may_act")
            })
            .transpose()?;
        Ok(Self {
            origin,
            actors,
            may_act,
        })
    }
}

impl Serialize for ActorChain {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_json_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ActorChain {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_json_value(&value).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<&serde_json::Value> for ActorChain {
    type Error = ActorChainError;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        Self::from_json_value(value)
    }
}

impl From<&ActorChain> for VmValue {
    fn from(chain: &ActorChain) -> Self {
        chain.to_vm_value()
    }
}

impl From<ActorChain> for VmValue {
    fn from(chain: ActorChain) -> Self {
        chain.to_vm_value()
    }
}

impl TryFrom<&VmValue> for ActorChain {
    type Error = ActorChainError;

    fn try_from(value: &VmValue) -> Result<Self, Self::Error> {
        Self::from_vm_value(value)
    }
}

pub struct ActorChainIter<'a> {
    inner: ActorChainEntryIter<'a>,
}

impl<'a> Iterator for ActorChainIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(ActorChainEntry::subject)
    }
}

impl<'a> IntoIterator for &'a ActorChain {
    type Item = &'a str;
    type IntoIter = ActorChainIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct ActorChainEntryIter<'a> {
    actors: std::slice::Iter<'a, ActorChainEntry>,
    origin: Option<&'a ActorChainEntry>,
}

impl<'a> Iterator for ActorChainEntryIter<'a> {
    type Item = &'a ActorChainEntry;

    fn next(&mut self) -> Option<Self::Item> {
        self.actors.next().or_else(|| self.origin.take())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActorChainError {
    message: String,
}

impl ActorChainError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ActorChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for ActorChainError {}

fn actor_nodes_to_json(actors: &[ActorChainEntry]) -> Option<serde_json::Value> {
    let mut next = None;
    for actor in actors.iter().rev() {
        let mut node = principal_json_object(actor);
        if let Some(act) = next {
            node.insert(ACT.to_string(), act);
        }
        next = Some(serde_json::Value::Object(node));
    }
    next
}

fn principal_json_object(entry: &ActorChainEntry) -> serde_json::Map<String, serde_json::Value> {
    let mut claim = serde_json::Map::new();
    claim.insert(
        SUB.to_string(),
        serde_json::Value::String(entry.subject.clone()),
    );
    if !entry.scopes.is_empty() {
        claim.insert(
            SCOPES.to_string(),
            serde_json::Value::Array(
                entry
                    .scopes
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    claim
}

fn principal_claim_json(subject: &str) -> serde_json::Value {
    let mut claim = serde_json::Map::new();
    claim.insert(
        SUB.to_string(),
        serde_json::Value::String(subject.to_string()),
    );
    serde_json::Value::Object(claim)
}

fn expect_json_object<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, ActorChainError> {
    value
        .as_object()
        .ok_or_else(|| ActorChainError::new(format!("{path} must be an object")))
}

fn json_sub(
    object: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<String, ActorChainError> {
    object
        .get(SUB)
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| ActorChainError::new(format!("{path}.sub must be a string")))
}

fn json_entry(
    object: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<ActorChainEntry, ActorChainError> {
    Ok(ActorChainEntry {
        subject: json_sub(object, path)?,
        scopes: json_scopes(object, path)?,
    })
}

fn json_scopes(
    object: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<BTreeSet<String>, ActorChainError> {
    if let Some(scopes) = object.get(SCOPES) {
        let items = scopes
            .as_array()
            .ok_or_else(|| ActorChainError::new(format!("{path}.scopes must be a list")))?;
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                item.as_str()
                    .ok_or_else(|| {
                        ActorChainError::new(format!("{path}.scopes[{index}] must be a string"))
                    })
                    .map(str::to_string)
            })
            .collect::<Result<Vec<_>, _>>()
            .map(normalize_scopes);
    }
    if let Some(scope) = object.get(SCOPE) {
        let raw = scope
            .as_str()
            .ok_or_else(|| ActorChainError::new(format!("{path}.scope must be a string")))?;
        return Ok(normalize_scopes(raw.split_whitespace()));
    }
    Ok(BTreeSet::new())
}

fn json_actor_subjects(
    first: Option<&serde_json::Value>,
    first_path: &str,
) -> Result<Vec<ActorChainEntry>, ActorChainError> {
    let mut actors = Vec::new();
    let mut current = first;
    let mut path = first_path.to_string();
    while let Some(value) = current {
        let object = expect_json_object(value, &path)?;
        actors.push(json_entry(object, &path)?);
        current = object.get(ACT);
        path.push_str(".act");
    }
    Ok(actors)
}

fn expect_vm_dict<'a>(value: &'a VmValue, path: &str) -> Result<&'a DictMap, ActorChainError> {
    value
        .as_dict()
        .ok_or_else(|| ActorChainError::new(format!("{path} must be a dict")))
}

fn vm_sub(object: &DictMap, path: &str) -> Result<String, ActorChainError> {
    match object.get(SUB) {
        Some(VmValue::String(subject)) => Ok(subject.to_string()),
        _ => Err(ActorChainError::new(format!("{path}.sub must be a string"))),
    }
}

fn vm_entry(object: &DictMap, path: &str) -> Result<ActorChainEntry, ActorChainError> {
    Ok(ActorChainEntry {
        subject: vm_sub(object, path)?,
        scopes: vm_scopes(object, path)?,
    })
}

fn vm_scopes(object: &DictMap, path: &str) -> Result<BTreeSet<String>, ActorChainError> {
    if let Some(scopes) = object.get(SCOPES) {
        let VmValue::List(items) = scopes else {
            return Err(ActorChainError::new(format!(
                "{path}.scopes must be a list"
            )));
        };
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| match item {
                VmValue::String(scope) => Ok(scope.to_string()),
                _ => Err(ActorChainError::new(format!(
                    "{path}.scopes[{index}] must be a string"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(normalize_scopes);
    }
    if let Some(scope) = object.get(SCOPE) {
        let VmValue::String(raw) = scope else {
            return Err(ActorChainError::new(format!(
                "{path}.scope must be a string"
            )));
        };
        return Ok(normalize_scopes(raw.split_whitespace()));
    }
    Ok(BTreeSet::new())
}

fn vm_actor_subjects(
    first: Option<&VmValue>,
    first_path: &str,
) -> Result<Vec<ActorChainEntry>, ActorChainError> {
    let mut actors = Vec::new();
    let mut current = first;
    let mut path = first_path.to_string();
    while let Some(value) = current {
        let object = expect_vm_dict(value, &path)?;
        actors.push(vm_entry(object, &path)?);
        current = object.get(ACT);
        path.push_str(".act");
    }
    Ok(actors)
}

fn normalize_scopes<I, S>(scopes: I) -> BTreeSet<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    scopes
        .into_iter()
        .map(Into::into)
        .map(|scope| scope.trim().to_string())
        .filter(|scope| !scope.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn principal() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-z][a-z0-9_]{0,24}").unwrap()
    }

    fn actor_subjects_from_json(value: &serde_json::Value) -> Vec<String> {
        let mut subjects = Vec::new();
        let mut current = value.get(ACT);
        while let Some(node) = current {
            subjects.push(node[SUB].as_str().expect("act.sub").to_string());
            current = node.get(ACT);
        }
        subjects
    }

    #[test]
    fn push_makes_new_actor_current_and_serializes_rfc_shape() {
        let chain = ActorChain::new("user")
            .pushed("service77")
            .pushed("service16");

        assert_eq!(chain.origin(), "user");
        assert_eq!(chain.current(), "service16");
        assert_eq!(
            chain.iter().collect::<Vec<_>>(),
            vec!["service16", "service77", "user"]
        );
        assert_eq!(
            serde_json::to_value(&chain).unwrap(),
            json!({
                "sub": "user",
                "act": {
                    "sub": "service16",
                    "act": {
                        "sub": "service77"
                    }
                }
            })
        );
    }

    #[test]
    fn may_act_serializes_as_authorized_actor_claim() {
        let chain = ActorChain::new("user").with_may_act("admin");

        assert_eq!(
            serde_json::to_value(&chain).unwrap(),
            json!({
                "sub": "user",
                "may_act": {
                    "sub": "admin"
                }
            })
        );
    }

    #[test]
    fn vm_value_conversion_round_trips_plain_harn_dicts() {
        let value = crate::schema::json_to_vm_value(&json!({
            "sub": "user",
            "act": {
                "sub": "service16",
                "act": {
                    "sub": "service77"
                }
            },
            "may_act": {
                "sub": "admin"
            }
        }));

        let chain = ActorChain::try_from(&value).unwrap();
        assert_eq!(chain.origin(), "user");
        assert_eq!(chain.current(), "service16");
        assert_eq!(
            chain.actors().collect::<Vec<_>>(),
            vec!["service16", "service77"]
        );
        assert_eq!(chain.may_act(), Some("admin"));

        let encoded = chain.to_vm_value();
        assert_eq!(ActorChain::try_from(&encoded).unwrap(), chain);
    }

    #[test]
    fn scoped_entries_round_trip_and_accept_rfc_scope_string() {
        let value = json!({
            "sub": "user:kenneth",
            "scope": "repo:read repo:write",
            "act": {
                "sub": "agent:burin",
                "scopes": ["repo:read", "repo:read"],
                "act": {
                    "sub": "agent:merge-captain",
                    "scope": "repo:read"
                }
            }
        });

        let chain = ActorChain::from_json_value(&value).unwrap();
        assert_eq!(
            chain.origin_entry().scopes().collect::<Vec<_>>(),
            vec!["repo:read", "repo:write"]
        );
        assert_eq!(
            chain.current_entry().scopes().collect::<Vec<_>>(),
            vec!["repo:read"]
        );

        assert_eq!(
            chain.to_json_value(),
            json!({
                "sub": "user:kenneth",
                "scopes": ["repo:read", "repo:write"],
                "act": {
                    "sub": "agent:burin",
                    "scopes": ["repo:read"],
                    "act": {
                        "sub": "agent:merge-captain",
                        "scopes": ["repo:read"]
                    }
                }
            })
        );
    }

    #[test]
    fn default_scope_attenuation_allows_equal_or_narrower_scopes_only() {
        let policy = ScopeAttenuationPolicy::default();
        let valid = ActorChain::new_with_scopes("user", ["repo:read", "repo:write"])
            .pushed_with_scopes("agent:burin", ["repo:read", "repo:write"])
            .pushed_with_scopes("agent:merge-captain", ["repo:read"]);
        valid.validate_scope_attenuation(&policy).unwrap();

        let invalid = ActorChain::new_with_scopes("user", ["repo:read"])
            .pushed_with_scopes("agent:burin", ["repo:read", "repo:write"]);
        let violation = invalid.validate_scope_attenuation(&policy).unwrap_err();
        assert_eq!(violation.parent_subject(), "user");
        assert_eq!(violation.child_subject(), "agent:burin");
        assert_eq!(violation.extra_scopes(), &["repo:write".to_string()]);
    }

    #[test]
    fn strict_scope_attenuation_requires_each_hop_to_shrink() {
        let policy = ScopeAttenuationPolicy {
            mode: ScopeAttenuationMode::StrictSubset,
            ..ScopeAttenuationPolicy::default()
        };
        let equal = ActorChain::new_with_scopes("user", ["repo:read"])
            .pushed_with_scopes("agent:burin", ["repo:read"]);
        let violation = equal.validate_scope_attenuation(&policy).unwrap_err();
        assert!(violation.extra_scopes().is_empty());

        let narrower = ActorChain::new_with_scopes("user", ["repo:read", "repo:write"])
            .pushed_with_scopes("agent:burin", ["repo:read"]);
        narrower.validate_scope_attenuation(&policy).unwrap();
    }

    #[test]
    fn parent_chain_removes_current_actor_but_preserves_origin() {
        let parent = ActorChain::new("user").pushed("agent:root");
        let child = parent.clone().pushed("agent:child");

        assert_eq!(child.parent_chain(), Some(parent));
        assert!(ActorChain::new("user").parent_chain().is_none());
    }

    proptest! {
        #[test]
        fn serde_round_trip_preserves_nesting_order(
            origin in principal(),
            actors in proptest::collection::vec(principal(), 0..10),
            may_act in proptest::option::of(principal()),
        ) {
            let mut chain = ActorChain::from_parts(origin.clone(), actors.clone());
            if let Some(may_act) = may_act.as_deref() {
                chain.set_may_act(may_act);
            }

            let encoded = serde_json::to_value(&chain).unwrap();
            prop_assert_eq!(encoded[SUB].as_str(), Some(origin.as_str()));
            let encoded_actors = actor_subjects_from_json(&encoded);
            prop_assert_eq!(encoded_actors.as_slice(), actors.as_slice());
            prop_assert_eq!(
                encoded.get(MAY_ACT).and_then(|claim| claim[SUB].as_str()),
                may_act.as_deref()
            );

            let decoded: ActorChain = serde_json::from_value(encoded.clone()).unwrap();
            prop_assert_eq!(&decoded, &chain);
            prop_assert_eq!(serde_json::to_value(&decoded).unwrap(), encoded);
            prop_assert_eq!(decoded.origin(), origin.as_str());
            prop_assert_eq!(
                decoded.current(),
                actors.first().map(String::as_str).unwrap_or(origin.as_str())
            );

            let expected_iter = actors
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(origin.as_str()))
                .collect::<Vec<_>>();
            prop_assert_eq!(decoded.iter().collect::<Vec<_>>(), expected_iter);
        }
    }
}
