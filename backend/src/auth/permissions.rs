//! Permissions: global rights plus view/create/edit/delete per CI class.
//!
//! A user's effective permissions are the union of every profile they hold.
//! The built-in Administrator profile holds everything implicitly. Class
//! grants apply to exactly that class; the `all classes` wildcard applies to
//! every class, including ones created later.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Rights that are not tied to a CI class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
pub enum GlobalPermission {
    /// Create, edit, disable and delete users; reset passwords; assign profiles
    #[serde(rename = "users.manage")]
    UsersManage,
    /// Create, edit, clone and delete permission profiles
    #[serde(rename = "profiles.manage")]
    ProfilesManage,
    /// Change CI classes, attribute definitions, relationship types and rules, and lookups
    #[serde(rename = "datamodel.manage")]
    DatamodelManage,
    /// Change branding, navigation, dashboards and layouts
    #[serde(rename = "customization.manage")]
    CustomizationManage,
    /// Export and import the whole configuration
    #[serde(rename = "config.export_import")]
    ConfigExportImport,
    /// Read the audit log
    #[serde(rename = "audit.view")]
    AuditView,
    /// Import configuration items from CSV and Excel files (still limited by the class rights)
    #[serde(rename = "cis.import")]
    CisImport,
    /// Create, edit and delete views shared with all users
    #[serde(rename = "views.share")]
    ViewsShare,
}

impl GlobalPermission {
    pub const ALL: [GlobalPermission; 8] = [
        GlobalPermission::UsersManage,
        GlobalPermission::ProfilesManage,
        GlobalPermission::DatamodelManage,
        GlobalPermission::CustomizationManage,
        GlobalPermission::ConfigExportImport,
        GlobalPermission::AuditView,
        GlobalPermission::CisImport,
        GlobalPermission::ViewsShare,
    ];

    /// The value stored in permission_profile_global_permissions.permission.
    pub fn as_str(self) -> &'static str {
        match self {
            GlobalPermission::UsersManage => "users.manage",
            GlobalPermission::ProfilesManage => "profiles.manage",
            GlobalPermission::DatamodelManage => "datamodel.manage",
            GlobalPermission::CustomizationManage => "customization.manage",
            GlobalPermission::ConfigExportImport => "config.export_import",
            GlobalPermission::AuditView => "audit.view",
            GlobalPermission::CisImport => "cis.import",
            GlobalPermission::ViewsShare => "views.share",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

/// An operation on the CIs of one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassOp {
    View,
    Create,
    Edit,
    Delete,
}

impl ClassOp {
    pub fn as_str(self) -> &'static str {
        match self {
            ClassOp::View => "view",
            ClassOp::Create => "create",
            ClassOp::Edit => "edit",
            ClassOp::Delete => "delete",
        }
    }
}

/// What a grant allows on one class (or on every class).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClassRights {
    pub view: bool,
    pub create: bool,
    pub edit: bool,
    pub delete: bool,
}

impl ClassRights {
    pub const ALL: ClassRights = ClassRights { view: true, create: true, edit: true, delete: true };

    pub fn allows(self, op: ClassOp) -> bool {
        match op {
            ClassOp::View => self.view,
            ClassOp::Create => self.create,
            ClassOp::Edit => self.edit,
            ClassOp::Delete => self.delete,
        }
    }

    pub fn union(self, other: ClassRights) -> ClassRights {
        ClassRights {
            view: self.view || other.view,
            create: self.create || other.create,
            edit: self.edit || other.edit,
            delete: self.delete || other.delete,
        }
    }

    /// Write rights imply view (the database requires it on every row).
    pub fn normalised(self) -> ClassRights {
        ClassRights { view: self.view || self.create || self.edit || self.delete, ..self }
    }

    pub fn is_empty(self) -> bool {
        !(self.view || self.create || self.edit || self.delete)
    }
}

/// One user's effective permissions (the union over their profiles).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
    /// Holds the built-in Administrator profile: everything is allowed.
    pub administrator: bool,
    pub global: BTreeSet<GlobalPermission>,
    /// The `all classes` wildcard.
    pub all_classes: ClassRights,
    pub classes: BTreeMap<Uuid, ClassRights>,
}

impl Permissions {
    pub fn has(&self, p: GlobalPermission) -> bool {
        self.administrator || self.global.contains(&p)
    }

    pub fn can(&self, class_id: Uuid, op: ClassOp) -> bool {
        self.administrator || self.all_classes.allows(op) || self.classes.get(&class_id).is_some_and(|r| r.allows(op))
    }

    /// Classes the operation is allowed on: `None` means every class.
    pub fn class_scope(&self, op: ClassOp) -> Option<Vec<Uuid>> {
        if self.administrator || self.all_classes.allows(op) {
            return None;
        }
        Some(self.classes.iter().filter(|(_, r)| r.allows(op)).map(|(id, _)| *id).collect())
    }

    /// Whether everything `other` allows is allowed here too. A user manager
    /// or profile manager may only hand out (or act on accounts with) what they
    /// hold themselves, so neither permission is a path to more.
    pub fn covers(&self, other: &Permissions) -> bool {
        if self.administrator {
            return true;
        }
        if other.administrator {
            return false;
        }
        other.global.iter().all(|g| self.has(*g))
            && [ClassOp::View, ClassOp::Create, ClassOp::Edit, ClassOp::Delete].into_iter().all(|op| {
                (!other.all_classes.allows(op) || self.all_classes.allows(op))
                    && other.classes.iter().all(|(id, r)| !r.allows(op) || self.can(*id, op))
            })
    }

    /// What both allow: an API token gets its owner's permissions narrowed to
    /// its profile's, so it never grants more than the owner holds.
    pub fn intersect(&self, other: &Permissions) -> Permissions {
        if self.administrator {
            return other.clone();
        }
        if other.administrator {
            return self.clone();
        }
        let both = |id: Option<Uuid>| {
            let can = |p: &Permissions, op| match id {
                Some(id) => p.can(id, op),
                None => p.all_classes.allows(op),
            };
            ClassRights {
                view: can(self, ClassOp::View) && can(other, ClassOp::View),
                create: can(self, ClassOp::Create) && can(other, ClassOp::Create),
                edit: can(self, ClassOp::Edit) && can(other, ClassOp::Edit),
                delete: can(self, ClassOp::Delete) && can(other, ClassOp::Delete),
            }
        };
        let classes = self
            .classes
            .keys()
            .chain(other.classes.keys())
            .map(|id| (*id, both(Some(*id))))
            .filter(|(_, r)| !r.is_empty())
            .collect();
        Permissions {
            administrator: false,
            global: self.global.intersection(&other.global).copied().collect(),
            all_classes: both(None),
            classes,
        }
    }

    /// Adds one profile's grants.
    pub fn merge_global(&mut self, p: GlobalPermission) {
        self.global.insert(p);
    }

    pub fn merge_class(&mut self, class_id: Option<Uuid>, rights: ClassRights) {
        match class_id {
            None => self.all_classes = self.all_classes.union(rights),
            Some(id) => {
                let entry = self.classes.entry(id).or_default();
                *entry = entry.union(rights);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn global_permissions_round_trip_through_their_stored_names() {
        for p in GlobalPermission::ALL {
            assert_eq!(GlobalPermission::parse(p.as_str()), Some(p));
            assert_eq!(serde_json::to_value(p).unwrap(), serde_json::Value::String(p.as_str().into()));
        }
        assert_eq!(GlobalPermission::parse("users.delete"), None);
    }

    #[test]
    fn nothing_is_allowed_by_default() {
        let p = Permissions::default();
        assert!(!p.has(GlobalPermission::AuditView));
        assert!(!p.can(id(1), ClassOp::View));
        assert_eq!(p.class_scope(ClassOp::View), Some(vec![]));
    }

    #[test]
    fn administrator_is_allowed_everything() {
        let p = Permissions { administrator: true, ..Default::default() };
        assert!(GlobalPermission::ALL.iter().all(|g| p.has(*g)));
        assert!(p.can(id(1), ClassOp::Delete));
        assert_eq!(p.class_scope(ClassOp::Create), None);
    }

    #[test]
    fn class_grants_are_per_class_and_per_operation() {
        let mut p = Permissions::default();
        p.merge_class(Some(id(1)), ClassRights { view: true, edit: true, ..Default::default() });
        p.merge_class(Some(id(2)), ClassRights { view: true, ..Default::default() });
        assert!(p.can(id(1), ClassOp::Edit));
        assert!(!p.can(id(1), ClassOp::Delete));
        assert!(!p.can(id(2), ClassOp::Edit));
        assert!(!p.can(id(3), ClassOp::View));
        assert_eq!(p.class_scope(ClassOp::View), Some(vec![id(1), id(2)]));
        assert_eq!(p.class_scope(ClassOp::Edit), Some(vec![id(1)]));
    }

    #[test]
    fn profiles_combine_as_a_union_and_the_wildcard_covers_every_class() {
        let mut p = Permissions::default();
        p.merge_class(Some(id(1)), ClassRights { view: true, ..Default::default() });
        p.merge_class(Some(id(1)), ClassRights { view: true, delete: true, ..Default::default() });
        assert!(p.can(id(1), ClassOp::Delete));
        p.merge_class(None, ClassRights { view: true, ..Default::default() });
        assert!(p.can(id(99), ClassOp::View));
        assert!(!p.can(id(99), ClassOp::Create));
        assert_eq!(p.class_scope(ClassOp::View), None);
        assert_eq!(p.class_scope(ClassOp::Delete), Some(vec![id(1)]));
    }

    #[test]
    fn covers_is_a_subset_check() {
        let admin = Permissions { administrator: true, ..Default::default() };
        let mut manager = Permissions::default();
        manager.merge_global(GlobalPermission::UsersManage);
        manager.merge_class(Some(id(1)), ClassRights { view: true, edit: true, ..Default::default() });

        let mut smaller = Permissions::default();
        smaller.merge_class(Some(id(1)), ClassRights { view: true, ..Default::default() });
        assert!(manager.covers(&smaller));
        assert!(manager.covers(&Permissions::default()));
        assert!(admin.covers(&manager));

        assert!(!manager.covers(&admin), "only administrators hand out Administrator");
        let mut other_global = Permissions::default();
        other_global.merge_global(GlobalPermission::ProfilesManage);
        assert!(!manager.covers(&other_global));
        let mut other_class = Permissions::default();
        other_class.merge_class(Some(id(2)), ClassRights { view: true, ..Default::default() });
        assert!(!manager.covers(&other_class));
        let mut wildcard = Permissions::default();
        wildcard.merge_class(None, ClassRights { view: true, ..Default::default() });
        assert!(!manager.covers(&wildcard), "a class grant does not cover the wildcard");
        let mut all_view = manager.clone();
        all_view.merge_class(None, ClassRights { view: true, ..Default::default() });
        assert!(all_view.covers(&wildcard));
        assert!(all_view.covers(&other_class), "the wildcard covers a single class");
    }

    #[test]
    fn intersect_keeps_only_what_both_allow() {
        let admin = Permissions { administrator: true, ..Default::default() };
        let mut owner = Permissions::default();
        owner.merge_global(GlobalPermission::AuditView);
        owner.merge_global(GlobalPermission::UsersManage);
        owner.merge_class(None, ClassRights { view: true, ..Default::default() });
        owner.merge_class(Some(id(1)), ClassRights { view: true, edit: true, ..Default::default() });

        let mut scope = Permissions::default();
        scope.merge_global(GlobalPermission::AuditView);
        scope.merge_global(GlobalPermission::DatamodelManage);
        scope.merge_class(Some(id(1)), ClassRights::ALL);
        scope.merge_class(Some(id(2)), ClassRights { view: true, ..Default::default() });

        let t = owner.intersect(&scope);
        assert!(!t.administrator);
        assert!(t.has(GlobalPermission::AuditView));
        assert!(!t.has(GlobalPermission::UsersManage), "not in the scope");
        assert!(!t.has(GlobalPermission::DatamodelManage), "not held by the owner");
        assert!(t.can(id(1), ClassOp::Edit) && !t.can(id(1), ClassOp::Delete));
        assert!(t.can(id(2), ClassOp::View), "the owner's wildcard covers class 2");
        assert!(!t.can(id(3), ClassOp::View), "the scope has no wildcard");
        assert_eq!(t.class_scope(ClassOp::View), Some(vec![id(1), id(2)]));
        assert!(owner.covers(&t) && scope.covers(&t));

        assert_eq!(admin.intersect(&scope), scope, "an administrator owner gets the scope");
        assert_eq!(owner.intersect(&admin), owner, "the Administrator scope keeps the owner's rights");
        assert!(admin.intersect(&admin).administrator);
        assert_eq!(owner.intersect(&Permissions::default()), Permissions::default());
    }

    #[test]
    fn write_rights_imply_view() {
        let r = ClassRights { delete: true, ..Default::default() }.normalised();
        assert!(r.view && r.delete && !r.edit);
        assert!(ClassRights::default().normalised().is_empty());
    }
}
