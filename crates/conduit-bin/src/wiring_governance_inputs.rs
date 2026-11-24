//! Governance forms use the same fixed input types as the live API. Parsing a
//! form never invokes a resolver, service commit, or caller-selected operation.
use async_graphql::{
    InputType,
    registry::{MetaType, Registry},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct GraphInput<T>(pub T);
fn check_fields(registry: &Registry, ty: &str, value: &async_graphql::Value) -> Result<(), String> {
    if let async_graphql::Value::List(values) = value {
        for value in values {
            check_fields(
                registry,
                ty.trim_end_matches('!')
                    .trim_start_matches('[')
                    .trim_end_matches(']'),
                value,
            )?;
        }
        return Ok(());
    }
    if let (
        Some(MetaType::InputObject { input_fields, .. }),
        async_graphql::Value::Object(values),
    ) = (registry.types.get(ty.trim_end_matches('!')), value)
    {
        for (name, value) in values {
            let field = input_fields
                .get(name.as_str())
                .ok_or("unknown input field")?;
            check_fields(registry, &field.ty, value)?;
        }
    }
    Ok(())
}
impl<T: InputType> Serialize for GraphInput<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0
            .to_value()
            .into_json()
            .map_err(|_| serde::ser::Error::custom("invalid form"))?
            .serialize(s)
    }
}
impl<'de, T: InputType> Deserialize<'de> for GraphInput<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let json = Value::deserialize(d)?;
        let value = async_graphql::Value::from_json(json)
            .map_err(|_| serde::de::Error::custom("invalid form"))?;
        let mut registry = Registry::default();
        let ty = T::create_type_info(&mut registry);
        check_fields(&registry, &ty, &value)
            .map_err(|_| serde::de::Error::custom("invalid fixed input field"))?;
        T::parse(Some(value))
            .map(Self)
            .map_err(|_| serde::de::Error::custom("invalid fixed input"))
    }
}
