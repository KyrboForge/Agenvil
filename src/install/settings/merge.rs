use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::fragment::{Fragment, Hook, commands, pointer, tokens};
use super::state::{HookRecord, Previous, ValueRecord, hook_record};
use super::{EntryKind, Operation, OperationKind, Reason};
use crate::install::Ownership;

pub(super) struct Outcome {
    pub operations: Vec<Operation>,
    pub document: Value,
    pub values: Vec<ValueRecord>,
    pub hooks: Vec<HookRecord>,
}

/// The parent of a missing key is not an object.
struct Clash;

/// Reconcile `desired` against the document and the recorded ownership. Removal is an empty fragment.
pub(super) fn merge(
    document: &Value,
    values: &[ValueRecord],
    hooks: &[HookRecord],
    desired: &Fragment,
) -> Outcome {
    let mut out = Outcome {
        operations: Vec::new(),
        document: document.clone(),
        values: Vec::new(),
        hooks: Vec::new(),
    };
    let mut prune = BTreeSet::new();
    for record in values
        .iter()
        .filter(|record| !desired.values.iter().any(|(p, _)| *p == record.pointer))
    {
        remove_value(&mut out, record, &mut prune);
    }
    for (target, value) in &desired.values {
        let record = values.iter().find(|record| record.pointer == *target);
        set_value(&mut out, target, value, record);
    }
    for record in hooks
        .iter()
        .filter(|record| !desired.hooks.iter().any(|hook| hook.id == record.id))
    {
        remove_hook(&mut out, record, &mut prune);
    }
    for hook in &desired.hooks {
        let record = hooks.iter().find(|record| record.id == hook.id);
        set_hook(&mut out, hook, record);
    }
    // Deepest first, so a parent empties once its Agenvil-created children are gone.
    let mut prune: Vec<_> = prune.into_iter().filter_map(|p| tokens(&p)).collect();
    prune.sort_by_key(|parts| std::cmp::Reverse(parts.len()));
    for parts in prune {
        if let Ok(Some(Value::Object(map))) = lookup(&out.document, &parts)
            && !map.is_empty()
        {
            continue;
        }
        if let Ok(Some(Value::Array(items))) = lookup(&out.document, &parts)
            && !items.is_empty()
        {
            continue;
        }
        if matches!(
            lookup(&out.document, &parts),
            Ok(Some(Value::Object(_) | Value::Array(_)))
        ) {
            delete(&mut out.document, &parts);
        }
    }
    out
}

fn op(
    out: &mut Outcome,
    operation: OperationKind,
    kind: EntryKind,
    target: &str,
    event: Option<&str>,
    ownership: Ownership,
    reason: Reason,
) {
    out.operations.push(Operation {
        operation,
        kind,
        target: target.to_owned(),
        event: event.map(str::to_owned),
        ownership,
        reason,
    });
}

fn set_value(out: &mut Outcome, target: &str, desired: &Value, record: Option<&ValueRecord>) {
    use OperationKind::{Add, Change, Conflict, Noop};
    let parts = tokens(target).expect("validated fragment pointer");
    let value = |op, owner, why, out: &mut Outcome| {
        self::op(out, op, EntryKind::Value, target, None, owner, why);
    };
    let current = match lookup(&out.document, &parts) {
        Err(Clash) => {
            let owner = record.map_or(Ownership::Unmanaged, |_| Ownership::Mismatch);
            value(Conflict, owner, Reason::TypeClash, out);
            out.values.extend(record.cloned());
            return;
        }
        Ok(current) => current.cloned(),
    };
    match current {
        None => {
            let created = insert(&mut out.document, &parts, desired.clone());
            value(Add, Ownership::Absent, Reason::Missing, out);
            out.values.push(ValueRecord {
                pointer: target.to_owned(),
                value: desired.clone(),
                previous: None,
                created,
            });
        }
        Some(current) if current == *desired => {
            let managed = record.is_some_and(|record| record.value == current);
            let (owner, why) = if managed {
                (Ownership::Managed, Reason::Current)
            } else {
                (Ownership::Unmanaged, Reason::Adopted)
            };
            value(Noop, owner, why, out);
            out.values.push(match record {
                Some(record) if managed => record.clone(),
                // An equal user value is recorded as its own previous value, so removal keeps it.
                _ => ValueRecord {
                    pointer: target.to_owned(),
                    value: current.clone(),
                    previous: Some(Previous { value: current }),
                    created: Vec::new(),
                },
            });
        }
        Some(current) => match record {
            Some(record) if record.value == current => {
                insert(&mut out.document, &parts, desired.clone());
                value(Change, Ownership::Managed, Reason::Changed, out);
                out.values.push(ValueRecord {
                    value: desired.clone(),
                    ..record.clone()
                });
            }
            Some(record) => {
                value(Conflict, Ownership::Mismatch, Reason::UserEdited, out);
                out.values.push(record.clone());
            }
            None => value(Conflict, Ownership::Unmanaged, Reason::UserValue, out),
        },
    }
}

fn remove_value(out: &mut Outcome, record: &ValueRecord, prune: &mut BTreeSet<String>) {
    let parts = tokens(&record.pointer).expect("validated record pointer");
    let value = |op, owner, why, out: &mut Outcome| {
        self::op(out, op, EntryKind::Value, &record.pointer, None, owner, why);
    };
    match lookup(&out.document, &parts) {
        Err(Clash) | Ok(None) => value(OperationKind::Remove, Ownership::Absent, Reason::Gone, out),
        Ok(Some(current)) if *current == record.value => {
            match &record.previous {
                Some(previous) => {
                    insert(&mut out.document, &parts, previous.value.clone());
                }
                None => {
                    delete(&mut out.document, &parts);
                    prune.extend(record.created.iter().cloned());
                }
            }
            value(
                OperationKind::Remove,
                Ownership::Managed,
                Reason::Obsolete,
                out,
            );
        }
        Ok(Some(_)) => {
            value(
                OperationKind::Conflict,
                Ownership::Mismatch,
                Reason::UserEdited,
                out,
            );
            out.values.push(record.clone());
        }
    }
}

fn set_hook(out: &mut Outcome, hook: &Hook, record: Option<&HookRecord>) {
    use OperationKind::{Add, Change, Conflict, Noop};
    let report = |op, owner, why, out: &mut Outcome| {
        self::op(
            out,
            op,
            EntryKind::Hook,
            &hook.id,
            Some(&hook.event),
            owner,
            why,
        );
    };
    let Ok(current) = groups(&out.document, &hook.event).map(<[Value]>::to_vec) else {
        report(Conflict, Ownership::Unmanaged, Reason::TypeClash, out);
        out.hooks.extend(record.cloned());
        return;
    };
    if let Some(record) = record {
        let Ok(old) = groups(&out.document, &record.event).map(<[Value]>::to_vec) else {
            report(Conflict, Ownership::Mismatch, Reason::TypeClash, out);
            out.hooks.push(record.clone());
            return;
        };
        if let Some(index) = old.iter().position(|group| *group == record.group) {
            if record.event == hook.event && record.group == hook.group {
                report(Noop, Ownership::Managed, Reason::Current, out);
                out.hooks.push(record.clone());
                return;
            }
            let mut created = record.created.clone();
            if record.event == hook.event {
                array(&mut out.document, &hook.event, &mut created)[index] = hook.group.clone();
            } else {
                array(&mut out.document, &record.event, &mut created).remove(index);
                array(&mut out.document, &hook.event, &mut created).push(hook.group.clone());
            }
            report(Change, Ownership::Managed, Reason::Changed, out);
            out.hooks
                .push(hook_record(&hook.id, &hook.event, &hook.group, created));
            return;
        }
        if old.iter().any(|group| same_commands(group, &record.group)) {
            report(Conflict, Ownership::Mismatch, Reason::HookModified, out);
            out.hooks.push(record.clone());
            return;
        }
    }
    if current
        .iter()
        .any(|group| *group == hook.group || same_commands(group, &hook.group))
    {
        report(Conflict, Ownership::Unmanaged, Reason::HookDuplicate, out);
        return;
    }
    let mut created = Vec::new();
    array(&mut out.document, &hook.event, &mut created).push(hook.group.clone());
    report(Add, Ownership::Absent, Reason::Missing, out);
    out.hooks
        .push(hook_record(&hook.id, &hook.event, &hook.group, created));
}

fn remove_hook(out: &mut Outcome, record: &HookRecord, prune: &mut BTreeSet<String>) {
    let report = |op, owner, why, out: &mut Outcome| {
        self::op(
            out,
            op,
            EntryKind::Hook,
            &record.id,
            Some(&record.event),
            owner,
            why,
        );
    };
    let current = groups(&out.document, &record.event)
        .map(<[Value]>::to_vec)
        .unwrap_or_default();
    if let Some(index) = current.iter().position(|group| *group == record.group) {
        array(&mut out.document, &record.event, &mut Vec::new()).remove(index);
        prune.extend(record.created.iter().cloned());
        report(
            OperationKind::Remove,
            Ownership::Managed,
            Reason::Obsolete,
            out,
        );
    } else if current
        .iter()
        .any(|group| same_commands(group, &record.group))
    {
        report(
            OperationKind::Conflict,
            Ownership::Mismatch,
            Reason::HookModified,
            out,
        );
        out.hooks.push(record.clone());
    } else {
        report(OperationKind::Remove, Ownership::Absent, Reason::Gone, out);
    }
}

fn same_commands(left: &Value, right: &Value) -> bool {
    let left = commands(left);
    !left.is_empty() && left == commands(right)
}

fn lookup<'a>(document: &'a Value, parts: &[String]) -> Result<Option<&'a Value>, Clash> {
    let mut current = document;
    for part in parts {
        let Value::Object(map) = current else {
            return Err(Clash);
        };
        match map.get(part) {
            Some(child) => current = child,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

/// The groups of one hook event; absent is empty, anything but an array is a clash.
fn groups<'a>(document: &'a Value, event: &str) -> Result<&'a [Value], Clash> {
    match lookup(document, &["hooks".to_owned(), event.to_owned()])? {
        None => Ok(&[]),
        Some(Value::Array(groups)) => Ok(groups),
        Some(_) => Err(Clash),
    }
}

/// Sets a key whose parents were checked, creating missing parent objects in place.
fn insert(document: &mut Value, parts: &[String], value: Value) -> Vec<String> {
    let mut created = Vec::new();
    let (last, parents) = parts.split_last().expect("pointers are nonempty");
    parent(document, parents, &mut created).insert(last.clone(), value);
    created
}

fn parent<'a>(
    document: &'a mut Value,
    parts: &[String],
    created: &mut Vec<String>,
) -> &'a mut Map<String, Value> {
    let mut current = document;
    for (depth, part) in parts.iter().enumerate() {
        let map = current.as_object_mut().expect("checked parent object");
        if !map.contains_key(part) {
            map.insert(part.clone(), Value::Object(Map::new()));
            created.push(pointer(&parts[..=depth]));
        }
        current = map.get_mut(part).expect("present");
    }
    current.as_object_mut().expect("checked parent object")
}

fn array<'a>(
    document: &'a mut Value,
    event: &str,
    created: &mut Vec<String>,
) -> &'a mut Vec<Value> {
    let parts = ["hooks".to_owned(), event.to_owned()];
    let hooks = parent(document, &parts[..1], created);
    if !hooks.contains_key(event) {
        hooks.insert(event.to_owned(), Value::Array(Vec::new()));
        created.push(pointer(&parts));
    }
    hooks[event].as_array_mut().expect("checked hook array")
}

fn delete(document: &mut Value, parts: &[String]) {
    let (last, parents) = parts.split_last().expect("pointers are nonempty");
    // `shift_remove` keeps the order of the remaining user keys.
    parent(document, parents, &mut Vec::new()).shift_remove(last);
}
