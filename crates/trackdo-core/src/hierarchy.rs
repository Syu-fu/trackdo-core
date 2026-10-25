//! Task hierarchy derived from the parent each task names.
//!
//! Every task stores its parent as an independent register, so replicas that
//! move tasks while offline can end up with a cycle once they sync. The
//! effective tree is derived deterministically from the registers, so every
//! replica shows the same tree without coordination.

use std::collections::{BTreeMap, BTreeSet};

use crate::hlc::Hlc;
use crate::task::{Task, TaskId};
use crate::time_entry::Timestamp;

/// The effective tree of a set of tasks.
///
/// - A parent that is unknown, or the task itself, is ignored, so the task
///   shows as a root until its parent arrives.
/// - In each cycle, the parent written last is ignored.
/// - A task counts as deleted when it or any ancestor is deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hierarchy {
    parents: BTreeMap<TaskId, TaskId>,
    children: BTreeMap<TaskId, Vec<TaskId>>,
    deleted_at: BTreeMap<TaskId, Timestamp>,
}

impl Hierarchy {
    pub fn derive(tasks: &BTreeMap<TaskId, Task>) -> Self {
        let parents = break_cycles(named_parents(tasks));
        let mut children: BTreeMap<TaskId, Vec<TaskId>> = BTreeMap::new();
        for (&child, &parent) in &parents {
            children.entry(parent).or_default().push(child);
        }
        let deleted_at = inherit_deletions(tasks, &parents);
        Self {
            parents,
            children,
            deleted_at,
        }
    }

    pub fn parent(&self, id: TaskId) -> Option<TaskId> {
        self.parents.get(&id).copied()
    }

    /// Effective children ordered by id, including deleted ones.
    pub fn children(&self, id: TaskId) -> &[TaskId] {
        self.children.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Parent, grandparent and so on, nearest first.
    pub fn ancestors(&self, id: TaskId) -> impl Iterator<Item = TaskId> + '_ {
        std::iter::successors(self.parent(id), |&ancestor| self.parent(ancestor))
    }

    /// Whether `id` is `ancestor` or lies below it.
    pub fn is_within(&self, id: TaskId, ancestor: TaskId) -> bool {
        id == ancestor || self.ancestors(id).any(|a| a == ancestor)
    }

    /// The task or one of its ancestors is deleted.
    pub fn is_deleted(&self, id: TaskId) -> bool {
        self.deleted_at.contains_key(&id)
    }

    /// When the task came to count as deleted: the earliest deletion, by
    /// wall time, of the task and its ancestors.
    pub fn deleted_at(&self, id: TaskId) -> Option<Timestamp> {
        self.deleted_at.get(&id).copied()
    }
}

/// Each task's named parent, when that parent is known, with the time the
/// parent was written.
fn named_parents(tasks: &BTreeMap<TaskId, Task>) -> BTreeMap<TaskId, (TaskId, Hlc)> {
    tasks
        .values()
        .filter_map(|task| {
            let register = task.registers().parent.as_ref()?;
            let parent = (*register.value()).filter(|parent| tasks.contains_key(parent))?;
            Some((task.id(), (parent, register.hlc())))
        })
        .collect()
}

/// Drops, in each cycle, the parent written last (ties broken by task id).
///
/// Every task names at most one parent, so cycles are disjoint and dropping
/// one parent per cycle leaves none.
fn break_cycles(named: BTreeMap<TaskId, (TaskId, Hlc)>) -> BTreeMap<TaskId, TaskId> {
    let mut visited = BTreeSet::new();
    let mut dropped: BTreeSet<TaskId> = BTreeSet::new();
    for &start in named.keys() {
        let mut path = Vec::new();
        let mut current = Some(start);
        while let Some(task) = current.filter(|task| !visited.contains(task)) {
            visited.insert(task);
            path.push(task);
            current = named.get(&task).map(|&(parent, _)| parent);
        }
        let cycle_start = current.and_then(|task| path.iter().position(|&t| t == task));
        if let Some(cycle) = cycle_start.map(|start| &path[start..]) {
            let newest = cycle.iter().max_by_key(|&&task| (named[&task].1, task));
            dropped.extend(newest);
        }
    }
    named
        .into_iter()
        .filter(|(task, _)| !dropped.contains(task))
        .map(|(task, (parent, _))| (task, parent))
        .collect()
}

/// For every task that is deleted or has a deleted ancestor, the earliest
/// such deletion by wall time.
fn inherit_deletions(
    tasks: &BTreeMap<TaskId, Task>,
    parents: &BTreeMap<TaskId, TaskId>,
) -> BTreeMap<TaskId, Timestamp> {
    let mut resolved: BTreeMap<TaskId, Option<Timestamp>> = BTreeMap::new();
    for &start in tasks.keys() {
        // Walk up to the first task already resolved, then resolve the path
        // top-down; the tree is acyclic, so the walk ends.
        let mut path = Vec::new();
        let mut current = Some(start);
        while let Some(task) = current.filter(|task| !resolved.contains_key(task)) {
            path.push(task);
            current = parents.get(&task).copied();
        }
        let mut inherited = current.and_then(|task| resolved[&task]);
        for task in path.into_iter().rev() {
            inherited = inherited
                .into_iter()
                .chain(own_deletion(&tasks[&task]))
                .min();
            resolved.insert(task, inherited);
        }
    }
    resolved
        .into_iter()
        .filter_map(|(task, deleted)| Some((task, deleted?)))
        .collect()
}

fn own_deletion(task: &Task) -> Option<Timestamp> {
    let register = task.registers().deleted.as_ref()?;
    task.is_deleted()
        .then(|| Timestamp(register.hlc().wall_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::DeviceId;
    use crate::task::TaskChange;
    use proptest::prelude::*;

    fn at(wall: u64) -> Hlc {
        Hlc::new(wall, 0, DeviceId(1))
    }

    fn id(n: u128) -> TaskId {
        TaskId(n)
    }

    /// Tasks `0..count` with no fields set.
    fn tasks(count: u128) -> BTreeMap<TaskId, Task> {
        (0..count).map(|n| (id(n), Task::empty(id(n)))).collect()
    }

    fn write(tasks: &mut BTreeMap<TaskId, Task>, task: u128, change: TaskChange, wall: u64) {
        let updated = tasks[&id(task)].apply(&change, at(wall));
        tasks.insert(id(task), updated);
    }

    fn set_parent(tasks: &mut BTreeMap<TaskId, Task>, child: u128, parent: u128, wall: u64) {
        write(tasks, child, TaskChange::Parent(Some(id(parent))), wall);
    }

    fn delete(tasks: &mut BTreeMap<TaskId, Task>, task: u128, wall: u64) {
        write(tasks, task, TaskChange::Deleted(true), wall);
    }

    #[test]
    fn known_parent_is_effective() {
        let mut tasks = tasks(3);
        set_parent(&mut tasks, 2, 0, 1);
        set_parent(&mut tasks, 1, 0, 1);

        let tree = Hierarchy::derive(&tasks);

        assert_eq!(tree.parent(id(1)), Some(id(0)));
        assert_eq!(tree.children(id(0)), &[id(1), id(2)]);
        assert_eq!(tree.parent(id(0)), None);
        assert_eq!(tree.children(id(1)), &[]);
    }

    #[test]
    fn unknown_parent_leaves_task_at_root() {
        let mut tasks = tasks(1);
        write(&mut tasks, 0, TaskChange::Parent(Some(id(99))), 1);

        assert_eq!(Hierarchy::derive(&tasks).parent(id(0)), None);
    }

    #[test]
    fn task_naming_itself_stays_at_root() {
        let mut tasks = tasks(1);
        set_parent(&mut tasks, 0, 0, 1);

        assert_eq!(Hierarchy::derive(&tasks).parent(id(0)), None);
    }

    #[test]
    fn cycle_drops_parent_written_last() {
        let mut tasks = tasks(4);
        set_parent(&mut tasks, 0, 1, 10);
        set_parent(&mut tasks, 1, 2, 30);
        set_parent(&mut tasks, 2, 0, 20);
        set_parent(&mut tasks, 3, 2, 99);

        let tree = Hierarchy::derive(&tasks);

        assert_eq!(tree.parent(id(1)), None);
        assert_eq!(tree.parent(id(0)), Some(id(1)));
        assert_eq!(tree.parent(id(2)), Some(id(0)));
        assert_eq!(tree.parent(id(3)), Some(id(2)));
    }

    #[test]
    fn ancestors_walk_up_to_the_root() {
        let mut tasks = tasks(3);
        set_parent(&mut tasks, 2, 1, 1);
        set_parent(&mut tasks, 1, 0, 1);

        let tree = Hierarchy::derive(&tasks);

        assert_eq!(tree.ancestors(id(2)).collect::<Vec<_>>(), [id(1), id(0)]);
        assert!(tree.is_within(id(2), id(0)));
        assert!(tree.is_within(id(0), id(0)));
        assert!(!tree.is_within(id(0), id(2)));
    }

    #[test]
    fn deleting_a_task_deletes_its_descendants_from_then_on() {
        let mut tasks = tasks(4);
        set_parent(&mut tasks, 1, 0, 1);
        set_parent(&mut tasks, 2, 1, 1);
        delete(&mut tasks, 1, 50);
        delete(&mut tasks, 2, 80);

        let tree = Hierarchy::derive(&tasks);

        assert!(!tree.is_deleted(id(0)));
        assert_eq!(tree.deleted_at(id(1)), Some(Timestamp(50)));
        assert_eq!(tree.deleted_at(id(2)), Some(Timestamp(50)));
        assert!(!tree.is_deleted(id(3)));
    }

    #[test]
    fn restoring_a_parent_restores_its_descendants() {
        let mut tasks = tasks(2);
        set_parent(&mut tasks, 1, 0, 1);
        delete(&mut tasks, 0, 50);
        write(&mut tasks, 0, TaskChange::Deleted(false), 60);

        assert!(!Hierarchy::derive(&tasks).is_deleted(id(1)));
    }

    fn any_tasks() -> impl Strategy<Value = BTreeMap<TaskId, Task>> {
        let one = (
            0u128..6,
            prop::option::of(0u128..7),
            0u64..20,
            any::<bool>(),
        );
        prop::collection::vec(one, 0..20).prop_map(|writes| {
            let mut tasks = tasks(6);
            for (task, parent, wall, deleted) in writes {
                write(&mut tasks, task, TaskChange::Parent(parent.map(id)), wall);
                if deleted {
                    delete(&mut tasks, task, wall);
                }
            }
            tasks
        })
    }

    proptest! {
        #[test]
        fn derived_tree_has_no_cycles(tasks in any_tasks()) {
            let tree = Hierarchy::derive(&tasks);

            for &task in tasks.keys() {
                prop_assert!(tree.ancestors(task).take(tasks.len() + 1).count() <= tasks.len());
                prop_assert!(!tree.ancestors(task).any(|a| a == task));
            }
        }

        #[test]
        fn children_mirror_parents(tasks in any_tasks()) {
            let tree = Hierarchy::derive(&tasks);

            for &task in tasks.keys() {
                for &child in tree.children(task) {
                    prop_assert_eq!(tree.parent(child), Some(task));
                }
                if let Some(parent) = tree.parent(task) {
                    prop_assert!(tree.children(parent).contains(&task));
                }
            }
        }

        #[test]
        fn descendants_of_deleted_tasks_are_deleted(tasks in any_tasks()) {
            let tree = Hierarchy::derive(&tasks);

            for &task in tasks.keys() {
                let own = tasks[&task].is_deleted();
                let inherited = tree.ancestors(task).any(|a| tasks[&a].is_deleted());
                prop_assert_eq!(tree.is_deleted(task), own || inherited);
            }
        }
    }
}
