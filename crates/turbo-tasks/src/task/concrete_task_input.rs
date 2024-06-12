use std::{
    any::Any,
    borrow::Cow,
    fmt::{Debug, Display},
    future::Future,
    hash::Hash,
    pin::Pin,
    sync::Arc,
};

use anyhow::Result;
use serde::{ser::SerializeTuple, Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    backend::CellContent,
    id::{FunctionId, TraitTypeId},
    magic_any::MagicAny,
    manager::{read_task_cell, read_task_output},
    registry, turbo_tasks, CellId, RawVc, RcStr, TaskId, TraitType, ValueTypeId,
};

/// A reference to a piece of data with optional type information
#[derive(Clone)]
pub struct SharedReference<T: TypeData>(pub T, pub Arc<dyn Any + Send + Sync>);

pub trait TypeData: Debug + PartialEq + Eq + Serialize + for<'de> Deserialize<'de> {
    fn type_id(&self) -> Option<ValueTypeId>;
}

impl TypeData for ValueTypeId {
    fn type_id(&self) -> Option<ValueTypeId> {
        Some(*self)
    }
}

impl TypeData for () {
    fn type_id(&self) -> Option<ValueTypeId> {
        None
    }
}

impl<T: TypeData> SharedReference<T> {
    pub fn downcast<Ty: Any + Send + Sync>(self) -> Option<Arc<Ty>> {
        match Arc::downcast(self.1) {
            Ok(data) => Some(data),
            Err(_) => None,
        }
    }

    pub(crate) fn typed(&self, type_id: ValueTypeId) -> SharedReference<ValueTypeId> {
        SharedReference(type_id, self.1.clone())
    }

    pub(crate) fn untyped(&self) -> SharedReference<()> {
        SharedReference((), self.1.clone())
    }
}

impl<T: TypeData> Hash for SharedReference<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Hash::hash(&(&*self.1 as *const (dyn Any + Send + Sync)), state)
    }
}
impl<T: TypeData> PartialEq for SharedReference<T> {
    // Must compare with PartialEq rather than std::ptr::addr_eq since the latter
    // only compares their addresses.
    #[allow(ambiguous_wide_pointer_comparisons)]
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(
            &*self.1 as *const (dyn Any + Send + Sync),
            &*other.1 as *const (dyn Any + Send + Sync),
        )
    }
}
impl<T: TypeData> Eq for SharedReference<T> {}
impl<T: TypeData> PartialOrd for SharedReference<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T: TypeData> Ord for SharedReference<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Ord::cmp(
            &(&*self.1 as *const (dyn Any + Send + Sync)).cast::<()>(),
            &(&*other.1 as *const (dyn Any + Send + Sync)).cast::<()>(),
        )
    }
}
impl<T: TypeData> Debug for SharedReference<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedReference")
            .field(&self.0)
            .field(&self.1)
            .finish()
    }
}

impl Serialize for SharedReference<ValueTypeId> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let SharedReference(ty, arc) = self;
        let value_type = registry::get_value_type(*ty);
        if let Some(serializable) = value_type.any_as_serializable(arc) {
            let mut t = serializer.serialize_tuple(2)?;
            t.serialize_element(registry::get_value_type_global_name(*ty))?;
            t.serialize_element(serializable)?;
            t.end()
        } else {
            Err(serde::ser::Error::custom(format!(
                "{:?} is not serializable",
                arc
            )))
        }
    }
}

impl<T: TypeData> Display for SharedReference<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ty) = self.0.type_id() {
            write!(f, "value of type {}", registry::get_value_type(ty).name)
        } else {
            write!(f, "untyped value")
        }
    }
}

impl<'de> Deserialize<'de> for SharedReference<ValueTypeId> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = SharedReference<ValueTypeId>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a serializable shared reference")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                if let Some(global_name) = seq.next_element()? {
                    if let Some(ty) = registry::get_value_type_id_by_global_name(global_name) {
                        if let Some(seed) = registry::get_value_type(ty).get_any_deserialize_seed()
                        {
                            if let Some(value) = seq.next_element_seed(seed)? {
                                Ok(SharedReference(ty, value.into()))
                            } else {
                                Err(serde::de::Error::invalid_length(
                                    1,
                                    &"tuple with type and value",
                                ))
                            }
                        } else {
                            Err(serde::de::Error::custom(format!(
                                "{ty} is not deserializable"
                            )))
                        }
                    } else {
                        Err(serde::de::Error::unknown_variant(global_name, &[]))
                    }
                } else {
                    Err(serde::de::Error::invalid_length(
                        0,
                        &"tuple with type and value",
                    ))
                }
            }
        }

        deserializer.deserialize_tuple(2, Visitor)
    }
}

#[derive(Debug, Clone, PartialOrd, Ord)]
pub struct TransientSharedValue(pub Arc<dyn MagicAny>);

impl TransientSharedValue {
    #[allow(dead_code)]
    pub fn downcast<T: MagicAny>(self) -> Option<Arc<T>> {
        match Arc::downcast(self.0.magic_any_arc()) {
            Ok(data) => Some(data),
            Err(_) => None,
        }
    }
}

impl Hash for TransientSharedValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialEq for TransientSharedValue {
    #[allow(clippy::op_ref)]
    fn eq(&self, other: &Self) -> bool {
        &self.0 == &other.0
    }
}
impl Eq for TransientSharedValue {}
impl Serialize for TransientSharedValue {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(serde::ser::Error::custom(
            "Transient values can't be serialized",
        ))
    }
}
impl<'de> Deserialize<'de> for TransientSharedValue {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        unreachable!("Transient values can't be serialized")
    }
}

#[derive(Debug, Clone, PartialOrd, Ord)]
pub struct SharedValue<T: TypeData>(pub T, pub Arc<dyn MagicAny>);

impl<T: TypeData> SharedValue<T> {
    pub fn downcast<Ty: Any + Send + Sync>(self) -> Option<Arc<Ty>> {
        match Arc::downcast(self.1.magic_any_arc()) {
            Ok(data) => Some(data),
            Err(_) => None,
        }
    }
}

impl<T: TypeData> PartialEq for SharedValue<T> {
    // this breaks without the ref
    #[allow(clippy::op_ref)]
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && &self.1 == &other.1
    }
}

impl<T: TypeData> Eq for SharedValue<T> {}

impl<T: TypeData + Hash> Hash for SharedValue<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
        self.1.hash(state);
    }
}

impl<T: TypeData> Display for SharedValue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ty) = self.0.type_id() {
            write!(f, "value of type {}", registry::get_value_type(ty).name)
        } else {
            write!(f, "untyped value")
        }
    }
}

impl Serialize for SharedValue<ValueTypeId> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let SharedValue(ty, arc) = self;
        let value_type = registry::get_value_type(*ty);
        if let Some(serializable) = value_type.magic_as_serializable(arc) {
            let mut t = serializer.serialize_tuple(2)?;
            t.serialize_element(registry::get_value_type_global_name(*ty))?;
            t.serialize_element(serializable)?;
            t.end()
        } else {
            Err(serde::ser::Error::custom(format!(
                "{:?} is not serializable",
                arc
            )))
        }
    }
}

impl<'de> Deserialize<'de> for SharedValue<ValueTypeId> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = SharedValue<ValueTypeId>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a serializable shared value")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                if let Some(global_name) = seq.next_element()? {
                    if let Some(ty) = registry::get_value_type_id_by_global_name(global_name) {
                        if let Some(seed) =
                            registry::get_value_type(ty).get_magic_deserialize_seed()
                        {
                            if let Some(value) = seq.next_element_seed(seed)? {
                                Ok(SharedValue(ty, value.into()))
                            } else {
                                Err(serde::de::Error::invalid_length(
                                    1,
                                    &"tuple with type and value",
                                ))
                            }
                        } else {
                            Err(serde::de::Error::custom(format!(
                                "{ty} is not deserializable"
                            )))
                        }
                    } else {
                        Err(serde::de::Error::unknown_variant(global_name, &[]))
                    }
                } else {
                    Err(serde::de::Error::invalid_length(
                        0,
                        &"tuple with type and value",
                    ))
                }
            }
        }

        deserializer.deserialize_tuple(2, Visitor)
    }
}

/// Intermediate representation of task inputs.
///
/// When a task is called, all its arguments will be converted and stored as
/// [`ConcreteTaskInput`]s. When the task is actually run, these inputs will be
/// converted back into the argument types. This is handled by the [`TaskInput`]
/// trait.
#[allow(clippy::derived_hash_with_manual_eq)]
#[derive(Debug, Hash, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum ConcreteTaskInput {
    TaskOutput(TaskId),
    TaskCell(TaskId, CellId),
    List(Vec<ConcreteTaskInput>),
    String(RcStr),
    Bool(bool),
    Usize(usize),
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    U64(u64),
    #[default]
    Nothing,
    SharedValue(SharedValue<ValueTypeId>),
    #[serde(
        serialize_with = "serialize_shared_value",
        deserialize_with = "deserialize_shared_value"
    )]
    TransientSharedValue(SharedValue<()>),
    SharedReference(SharedReference<ValueTypeId>),
    #[serde(
        serialize_with = "serialize_shared_ref",
        deserialize_with = "deserialize_shared_ref"
    )]
    TransientSharedReference(SharedReference<()>),
}

fn serialize_shared_value<S>(_: &SharedValue<()>, _: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    unreachable!("programmer error")
}

fn deserialize_shared_value<'de, D>(_: D) -> Result<SharedValue<()>, D::Error>
where
    D: Deserializer<'de>,
{
    unreachable!("programmer error")
}

fn serialize_shared_ref<S>(_: &SharedReference<()>, _: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    unreachable!("programmer error")
}

fn deserialize_shared_ref<'de, D>(_: D) -> Result<SharedReference<()>, D::Error>
where
    D: Deserializer<'de>,
{
    unreachable!("programmer error")
}

impl ConcreteTaskInput {
    pub async fn resolve_to_value(self) -> Result<ConcreteTaskInput> {
        let tt = turbo_tasks();
        let mut current = self;
        loop {
            current = match current {
                ConcreteTaskInput::TaskOutput(task_id) => {
                    read_task_output(&*tt, task_id, false).await?.into()
                }
                ConcreteTaskInput::TaskCell(task_id, index) => {
                    read_task_cell(&*tt, task_id, index).await?.into()
                }
                _ => return Ok(current),
            }
        }
    }

    pub async fn resolve(self) -> Result<ConcreteTaskInput> {
        let tt = turbo_tasks();
        let mut current = self;
        loop {
            current = match current {
                ConcreteTaskInput::TaskOutput(task_id) => {
                    read_task_output(&*tt, task_id, false).await?.into()
                }
                ConcreteTaskInput::List(list) => {
                    if list.iter().all(|i| i.is_resolved()) {
                        return Ok(ConcreteTaskInput::List(list));
                    }
                    fn resolve_all(
                        list: Vec<ConcreteTaskInput>,
                    ) -> Pin<Box<dyn Future<Output = Result<Vec<ConcreteTaskInput>>> + Send>>
                    {
                        use crate::TryJoinIterExt;
                        Box::pin(list.into_iter().map(|i| i.resolve()).try_join())
                    }
                    return Ok(ConcreteTaskInput::List(resolve_all(list).await?));
                }
                _ => return Ok(current),
            }
        }
    }

    pub fn get_task_id(&self) -> Option<TaskId> {
        match self {
            ConcreteTaskInput::TaskOutput(t) | ConcreteTaskInput::TaskCell(t, _) => Some(*t),
            _ => None,
        }
    }

    pub fn get_trait_method(
        &self,
        trait_type: TraitTypeId,
        name: Cow<'static, str>,
    ) -> Result<FunctionId, Cow<'static, str>> {
        match self {
            ConcreteTaskInput::TaskOutput(_) | ConcreteTaskInput::TaskCell(_, _) => {
                panic!("get_trait_method must be called on a resolved TaskInput")
            }
            ConcreteTaskInput::SharedValue(SharedValue(ty, _))
            | ConcreteTaskInput::SharedReference(SharedReference(ty, _)) => {
                let key = (trait_type, name);
                if let Some(func) = registry::get_value_type(*ty).get_trait_method(&key) {
                    Ok(*func)
                } else if let Some(func) = registry::get_trait(trait_type)
                    .default_trait_methods
                    .get(&key.1)
                {
                    Ok(*func)
                } else {
                    Err(key.1)
                }
            }
            _ => Err(name),
        }
    }

    pub fn has_trait(&self, trait_type: TraitTypeId) -> bool {
        match self {
            ConcreteTaskInput::TaskOutput(_) | ConcreteTaskInput::TaskCell(_, _) => {
                panic!("has_trait() must be called on a resolved TaskInput")
            }
            ConcreteTaskInput::SharedValue(SharedValue(ty, _))
            | ConcreteTaskInput::SharedReference(SharedReference(ty, _)) => {
                registry::get_value_type(*ty).has_trait(&trait_type)
            }
            _ => false,
        }
    }

    pub fn traits(&self) -> Vec<&'static TraitType> {
        match self {
            ConcreteTaskInput::TaskOutput(_) | ConcreteTaskInput::TaskCell(_, _) => {
                panic!("traits() must be called on a resolved TaskInput")
            }
            ConcreteTaskInput::SharedValue(SharedValue(ty, _))
            | ConcreteTaskInput::SharedReference(SharedReference(ty, _)) => {
                registry::get_value_type(*ty)
                    .traits_iter()
                    .map(registry::get_trait)
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    pub fn is_resolved(&self) -> bool {
        match self {
            ConcreteTaskInput::TaskOutput(_) => false,
            ConcreteTaskInput::List(list) => list.iter().all(|i| i.is_resolved()),
            _ => true,
        }
    }

    pub fn is_nothing(&self) -> bool {
        matches!(self, ConcreteTaskInput::Nothing)
    }
}

impl From<RawVc> for ConcreteTaskInput {
    fn from(raw_vc: RawVc) -> Self {
        match raw_vc {
            RawVc::TaskOutput(task) => ConcreteTaskInput::TaskOutput(task),
            RawVc::TaskCell(task, i) => ConcreteTaskInput::TaskCell(task, i),
        }
    }
}

impl From<CellContent<ValueTypeId>> for ConcreteTaskInput {
    fn from(content: CellContent<ValueTypeId>) -> Self {
        match content {
            CellContent(None) => ConcreteTaskInput::Nothing,
            CellContent(Some(shared_ref)) => ConcreteTaskInput::SharedReference(shared_ref),
        }
    }
}

impl Display for ConcreteTaskInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConcreteTaskInput::TaskOutput(task) => write!(f, "task output {}", task),
            ConcreteTaskInput::TaskCell(task, index) => write!(f, "cell {} in {}", index, task),
            ConcreteTaskInput::List(list) => write!(
                f,
                "list {}",
                list.iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ConcreteTaskInput::String(s) => write!(f, "string {:?}", s),
            ConcreteTaskInput::Bool(b) => write!(f, "bool {:?}", b),
            ConcreteTaskInput::Usize(v) => write!(f, "usize {}", v),
            ConcreteTaskInput::I8(v) => write!(f, "i8 {}", v),
            ConcreteTaskInput::U8(v) => write!(f, "u8 {}", v),
            ConcreteTaskInput::I16(v) => write!(f, "i16 {}", v),
            ConcreteTaskInput::U16(v) => write!(f, "u16 {}", v),
            ConcreteTaskInput::I32(v) => write!(f, "i32 {}", v),
            ConcreteTaskInput::U32(v) => write!(f, "u32 {}", v),
            ConcreteTaskInput::U64(v) => write!(f, "u64 {}", v),
            ConcreteTaskInput::Nothing => write!(f, "nothing"),
            ConcreteTaskInput::SharedValue(_) => write!(f, "any value"),
            ConcreteTaskInput::TransientSharedValue(_) => write!(f, "any transient value"),
            ConcreteTaskInput::SharedReference(data) => {
                write!(f, "shared reference with {}", data)
            }
            ConcreteTaskInput::TransientSharedReference(data) => {
                write!(f, "transient shared reference with {}", data)
            }
        }
    }
}
