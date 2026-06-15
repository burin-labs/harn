//! RFC 8693 actor/principal chain support.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::value::{DictMap, VmValue};

const SUB: &str = "sub";
const ACT: &str = "act";
const MAY_ACT: &str = "may_act";

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
    origin: String,
    actors: Vec<String>,
    may_act: Option<String>,
}

/// Alias for APIs that name the full acting chain as a principal.
pub type Principal = ActorChain;

impl ActorChain {
    /// Create a chain with only an originating subject and no acting agents.
    pub fn new(origin: impl Into<String>) -> Self {
        Self {
            origin: origin.into(),
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
            origin: origin.into(),
            actors: actors.into_iter().map(Into::into).collect(),
            may_act: None,
        }
    }

    /// Insert `actor` as the new current actor.
    pub fn push(&mut self, actor: impl Into<String>) {
        self.actors.insert(0, actor.into());
    }

    /// Chainable form of [`ActorChain::push`].
    pub fn pushed(mut self, actor: impl Into<String>) -> Self {
        self.push(actor);
        self
    }

    /// The actor that authorization checks should consider authoritative.
    ///
    /// When no delegation has occurred, the originating subject is also the
    /// current actor.
    pub fn current(&self) -> &str {
        self.actors
            .first()
            .map(String::as_str)
            .unwrap_or(self.origin.as_str())
    }

    /// The originating human principal carried in the top-level `sub` claim.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Acting agents in RFC nested-`act` order: current actor first.
    pub fn actors(&self) -> impl Iterator<Item = &str> {
        self.actors.iter().map(String::as_str)
    }

    /// All principals from current actor through the originating subject.
    pub fn iter(&self) -> ActorChainIter<'_> {
        ActorChainIter {
            actors: self.actors.iter(),
            origin: Some(self.origin.as_str()),
        }
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

    pub fn to_json_value(&self) -> serde_json::Value {
        let mut root = serde_json::Map::new();
        root.insert(
            SUB.to_string(),
            serde_json::Value::String(self.origin.clone()),
        );
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
        let origin = json_sub(root, "$")?;
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
        let origin = vm_sub(root, "$")?;
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
    actors: std::slice::Iter<'a, String>,
    origin: Option<&'a str>,
}

impl<'a> Iterator for ActorChainIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        self.actors
            .next()
            .map(String::as_str)
            .or_else(|| self.origin.take())
    }
}

impl<'a> IntoIterator for &'a ActorChain {
    type Item = &'a str;
    type IntoIter = ActorChainIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
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

fn actor_nodes_to_json(actors: &[String]) -> Option<serde_json::Value> {
    let mut next = None;
    for actor in actors.iter().rev() {
        let mut node = serde_json::Map::new();
        node.insert(SUB.to_string(), serde_json::Value::String(actor.clone()));
        if let Some(act) = next {
            node.insert(ACT.to_string(), act);
        }
        next = Some(serde_json::Value::Object(node));
    }
    next
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

fn json_actor_subjects(
    first: Option<&serde_json::Value>,
    first_path: &str,
) -> Result<Vec<String>, ActorChainError> {
    let mut actors = Vec::new();
    let mut current = first;
    let mut path = first_path.to_string();
    while let Some(value) = current {
        let object = expect_json_object(value, &path)?;
        actors.push(json_sub(object, &path)?);
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

fn vm_actor_subjects(
    first: Option<&VmValue>,
    first_path: &str,
) -> Result<Vec<String>, ActorChainError> {
    let mut actors = Vec::new();
    let mut current = first;
    let mut path = first_path.to_string();
    while let Some(value) = current {
        let object = expect_vm_dict(value, &path)?;
        actors.push(vm_sub(object, &path)?);
        current = object.get(ACT);
        path.push_str(".act");
    }
    Ok(actors)
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
