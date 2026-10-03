-- Users and Person CIs (SHAA-1505 decisions 1-10, SHAA-1508).
--
-- Every sign-in account (cmdb.users) is linked 1:1 to a CI of the built-in
-- Person class (ci_classes.system_role = 'person'). The link is stored in
-- users.person_ci_id; the user's e-mail is the source of truth and the
-- Person's Email field follows it.
--
-- The Person class:
--   * a new type "Person" (key person, person_2, ... if taken) in a new area
--     "People" (key people, suffixed if taken), with the fields Name (required,
--     the title), Email (required), Department, Phone and Job title. Name and
--     Email carry ci_attribute_definitions.system_role ('person_name',
--     'person_email'): they cannot be archived, made optional, change type or
--     be deleted. Administrators may add fields as usual.
--   * The area's schema, the type's table and the unique index on
--     lower(email) (uq_<field id hex>, the database-level uniqueness of Person
--     e-mails, ignoring case) are built by the reconcile that
--     `shadoucmdb migrate` runs right after the migrations: migrations never
--     build type tables (sql/README.md). `migrate` then links every user with
--     an e-mail to the Person with that e-mail, creating one where none
--     exists, in the same transaction as the reconcile.
--   * The class is protected like the business service class (0033): it
--     cannot be deleted, archived, made abstract, given a parent or subtypes.
--
-- users:
--   * person_ci_id: unique foreign key to configuration_items, ON DELETE
--     RESTRICT (a Person linked to a user cannot be purged; deleting the user
--     leaves the Person in place).
--   * e-mails are unique ignoring case (users_email_uq). If several users
--     share an e-mail, this migration stops and lists them; nothing changes
--     until an administrator gives each account its own address.
--   * the column stays nullable: accounts created before this release without
--     an e-mail must enter one at their next sign-in (403 EMAIL_REQUIRED on
--     every other route), so the bootstrap administrator is not locked out.
--   * users_person_link (deferred): a user with an e-mail points at a live CI
--     of the Person class, and a user without one points at nothing. Checked
--     on insert and on changes of email or person_ci_id, so the accounts that
--     wait for the link `migrate` makes are not refused on every sign-in.
--
-- configuration_items_keep_person: a Person linked to a user cannot be
-- deleted (soft delete) or change its type.
--
-- No new index on users beyond users_email_uq and the unique person_ci_id
-- (which also serves "the user of Person X").
--
-- Rollback: not supported in place, as for every release. Downgrading means
-- restoring the backup taken before the upgrade.

-- ---------------------------------------------------------------------------
-- Duplicate e-mails stop the upgrade, with the list
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  dups text;
BEGIN
  SELECT string_agg(format('%s (users: %s)', e.email, e.names), '; ' ORDER BY e.email) INTO dups
  FROM (
    SELECT lower(email) AS email, string_agg(username, ', ' ORDER BY lower(username)) AS names
    FROM cmdb.users WHERE email IS NOT NULL
    GROUP BY lower(email) HAVING count(*) > 1
  ) e;
  IF dups IS NOT NULL THEN
    RAISE EXCEPTION 'users: e-mail addresses must be unique (ignoring case) from this release on, and these are shared by several accounts: %. Give each account its own address (Administration > Users) with the previous release, then run `shadoucmdb migrate` again. Nothing was changed.', dups
      USING ERRCODE = 'unique_violation', CONSTRAINT = 'users_email_uq';
  END IF;
END;
$$;
--> statement-breakpoint

CREATE UNIQUE INDEX users_email_uq ON cmdb.users (lower(email));
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- System roles: the Person class and its key fields
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.ci_classes
  DROP CONSTRAINT ci_classes_system_role_valid,
  ADD CONSTRAINT ci_classes_system_role_valid CHECK (system_role IN ('business_service', 'person'));
--> statement-breakpoint
ALTER TABLE cmdb.ci_attribute_definitions
  ADD COLUMN system_role text,
  ADD CONSTRAINT ci_attribute_definitions_system_role_valid CHECK (system_role IN ('person_name', 'person_email')),
  ADD CONSTRAINT ci_attribute_definitions_system_role_uq UNIQUE (system_role);
--> statement-breakpoint

DO $$
DECLARE
  class_key text := 'person';
  area_key text := 'people';
  n integer := 1;
  new_area uuid;
  new_class uuid;
  name_field uuid;
BEGIN
  WHILE EXISTS (SELECT 1 FROM cmdb.areas WHERE key = area_key)
     OR EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = area_key) LOOP
    n := n + 1;
    area_key := 'people_' || n;
  END LOOP;
  INSERT INTO cmdb.areas (key, name, description, sort_order)
  VALUES (area_key, 'People', 'The people the configuration items refer to, and the sign-in accounts linked to them',
          COALESCE((SELECT max(sort_order) + 10 FROM cmdb.areas), 0))
  RETURNING id INTO new_area;

  n := 1;
  WHILE EXISTS (SELECT 1 FROM cmdb.ci_classes WHERE key = class_key) LOOP
    n := n + 1;
    class_key := 'person_' || n;
  END LOOP;
  -- A new type cannot take a system role (0033): except here.
  ALTER TABLE cmdb.ci_classes DISABLE TRIGGER ci_classes_keep_system_insert;
  INSERT INTO cmdb.ci_classes (key, name, description, area_id, color, icon, system_role)
  VALUES (class_key, 'Person',
          'A person: owner, contact or staff member. Every sign-in account is linked to one.',
          new_area, '#8250df', 'person', 'person')
  RETURNING id INTO new_class;
  ALTER TABLE cmdb.ci_classes ENABLE TRIGGER ci_classes_keep_system_insert;

  INSERT INTO cmdb.ci_attribute_definitions (class_id, key, label, data_type, is_required, validation, sort_order, system_role)
  VALUES (new_class, 'name', 'Name', 'text', true, '{"maxLength": 200, "pattern": "\\S"}', 0, 'person_name')
  RETURNING id INTO name_field;
  INSERT INTO cmdb.ci_attribute_definitions (class_id, key, label, data_type, is_required, validation, sort_order, system_role, help_text)
  VALUES (new_class, 'email', 'Email', 'text', true, '{"maxLength": 254, "pattern": "^[^@\\s]+@[^@\\s]+$"}', 10,
          'person_email', 'Unique across people. On a person linked to a sign-in account it follows the account''s e-mail.');
  INSERT INTO cmdb.ci_attribute_definitions (class_id, key, label, data_type, validation, sort_order) VALUES
    (new_class, 'department', 'Department', 'text', '{"maxLength": 200}', 20),
    (new_class, 'phone', 'Phone', 'text', '{"maxLength": 50}', 30),
    (new_class, 'job_title', 'Job title', 'text', '{"maxLength": 200}', 40);
  UPDATE cmdb.ci_classes SET title_attribute_id = name_field WHERE id = new_class;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Protections: the system classes (0033's function, with messages per role)
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.ci_classes_keep_system() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
DECLARE
  what text;
BEGIN
  IF TG_OP <> 'INSERT' THEN
    what := CASE OLD.system_role WHEN 'person' THEN 'built-in Person type' ELSE 'built-in business service type' END;
  END IF;
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'ci_classes: the % type is the % and cannot be deleted', OLD.key, what
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
  END IF;
  IF TG_OP = 'INSERT' AND NEW.system_role IS NOT NULL THEN
    RAISE EXCEPTION 'ci_classes: a new type cannot take a system role'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
  END IF;
  IF TG_OP = 'UPDATE' THEN
    IF NEW.system_role IS DISTINCT FROM OLD.system_role THEN
      RAISE EXCEPTION 'ci_classes: a type''s system role cannot change'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
    END IF;
    IF OLD.system_role IS NOT NULL THEN
      IF NOT NEW.is_active THEN
        RAISE EXCEPTION 'ci_classes: the % type is the % and cannot be archived', OLD.key, what
          USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
      END IF;
      IF NEW.is_abstract THEN
        RAISE EXCEPTION 'ci_classes: the % type is the % and cannot be abstract', OLD.key, what
          USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
      END IF;
      IF NEW.parent_id IS NOT NULL THEN
        RAISE EXCEPTION 'ci_classes: the % type is the % and cannot have a parent type', OLD.key, what
          USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
      END IF;
    END IF;
  END IF;
  IF NEW.parent_id IS NOT NULL
     AND EXISTS (SELECT 1 FROM ci_classes p WHERE p.id = NEW.parent_id AND p.system_role IS NOT NULL) THEN
    RAISE EXCEPTION 'ci_classes: built-in types (business service, Person) cannot have subtypes'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_no_subclass',
            TABLE = 'ci_classes', COLUMN = 'parent_id';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Protections: the Person's Name and Email fields
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.ci_attribute_definitions_keep_system() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
DECLARE
  fixed text;
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'ci_attribute_definitions: the % field is a key field of the built-in Person type and cannot be deleted', OLD.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_definitions_system_protected';
  END IF;
  IF TG_OP = 'INSERT' THEN
    RAISE EXCEPTION 'ci_attribute_definitions: a new field cannot take a system role'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_definitions_system_protected';
  END IF;
  IF NEW.system_role IS DISTINCT FROM OLD.system_role THEN
    RAISE EXCEPTION 'ci_attribute_definitions: a field''s system role cannot change'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_definitions_system_protected';
  END IF;
  IF OLD.system_role IS NOT NULL THEN
    fixed := CASE
      WHEN NEW.class_id IS DISTINCT FROM OLD.class_id THEN 'class_id'
      WHEN NEW.key IS DISTINCT FROM OLD.key THEN 'key'
      WHEN NEW.data_type IS DISTINCT FROM OLD.data_type THEN 'data_type'
      WHEN NEW.is_required IS DISTINCT FROM OLD.is_required THEN 'is_required'
      WHEN NEW.is_active IS DISTINCT FROM OLD.is_active THEN 'is_active'
    END;
    IF fixed IS NOT NULL THEN
      RAISE EXCEPTION 'ci_attribute_definitions: % of the % field of the built-in Person type cannot change', fixed, OLD.key
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_definitions_system_fixed',
              TABLE = 'ci_attribute_definitions', COLUMN = fixed;
    END IF;
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ci_attribute_definitions_keep_system
  BEFORE DELETE ON cmdb.ci_attribute_definitions
  FOR EACH ROW WHEN (OLD.system_role IS NOT NULL)
  EXECUTE FUNCTION cmdb.ci_attribute_definitions_keep_system();
--> statement-breakpoint
CREATE TRIGGER ci_attribute_definitions_keep_system_update
  BEFORE UPDATE OF system_role, class_id, key, data_type, is_required, is_active ON cmdb.ci_attribute_definitions
  FOR EACH ROW EXECUTE FUNCTION cmdb.ci_attribute_definitions_keep_system();
--> statement-breakpoint
CREATE TRIGGER ci_attribute_definitions_keep_system_insert
  BEFORE INSERT ON cmdb.ci_attribute_definitions
  FOR EACH ROW WHEN (NEW.system_role IS NOT NULL)
  EXECUTE FUNCTION cmdb.ci_attribute_definitions_keep_system();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- users.person_ci_id
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.users
  ADD COLUMN person_ci_id uuid,
  ADD CONSTRAINT users_person_ci_id_fk FOREIGN KEY (person_ci_id)
    REFERENCES cmdb.configuration_items (id) ON DELETE RESTRICT,
  ADD CONSTRAINT users_person_ci_id_uq UNIQUE (person_ci_id);
--> statement-breakpoint

CREATE FUNCTION cmdb.users_person_link() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
DECLARE
  u record;
BEGIN
  -- Deferred: read the row as it is at commit (it may have changed again, or gone).
  SELECT id, username, email, person_ci_id INTO u FROM users WHERE id = NEW.id;
  IF NOT FOUND THEN
    RETURN NULL;
  END IF;
  IF u.email IS NULL AND u.person_ci_id IS NOT NULL THEN
    RAISE EXCEPTION 'users: % has no e-mail address and cannot be linked to a person', u.username
      USING ERRCODE = 'check_violation', CONSTRAINT = 'users_person_link', TABLE = 'users', COLUMN = 'person_ci_id';
  END IF;
  IF u.email IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM configuration_items ci JOIN ci_classes c ON c.id = ci.class_id
    WHERE ci.id = u.person_ci_id AND c.system_role = 'person' AND ci.deleted_at IS NULL
  ) THEN
    RAISE EXCEPTION 'users: % must be linked to a live configuration item of the Person type', u.username
      USING ERRCODE = 'check_violation', CONSTRAINT = 'users_person_link', TABLE = 'users', COLUMN = 'person_ci_id';
  END IF;
  RETURN NULL;
END;
$$;
--> statement-breakpoint
CREATE CONSTRAINT TRIGGER users_person_link
  AFTER INSERT OR UPDATE OF email, person_ci_id ON cmdb.users
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION cmdb.users_person_link();
--> statement-breakpoint

-- A linked Person stays a live Person.
CREATE FUNCTION cmdb.configuration_items_keep_person() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
DECLARE
  owner text;
BEGIN
  SELECT username INTO owner FROM users WHERE person_ci_id = OLD.id;
  IF owner IS NULL THEN
    RETURN NEW;
  END IF;
  IF NEW.deleted_at IS NOT NULL AND OLD.deleted_at IS NULL THEN
    RAISE EXCEPTION 'configuration_items: this person is linked to the sign-in account %; disable or delete the account first', owner
      USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_person_linked',
            TABLE = 'configuration_items', COLUMN = 'deleted_at';
  END IF;
  IF NEW.class_id IS DISTINCT FROM OLD.class_id THEN
    RAISE EXCEPTION 'configuration_items: this person is linked to the sign-in account % and cannot change its type', owner
      USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_person_linked',
            TABLE = 'configuration_items', COLUMN = 'class_id';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER configuration_items_keep_person
  BEFORE UPDATE OF deleted_at, class_id ON cmdb.configuration_items
  FOR EACH ROW WHEN (NEW.deleted_at IS DISTINCT FROM OLD.deleted_at OR NEW.class_id IS DISTINCT FROM OLD.class_id)
  EXECUTE FUNCTION cmdb.configuration_items_keep_person();
