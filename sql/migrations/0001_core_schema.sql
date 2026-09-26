CREATE TABLE "environments" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"key" text NOT NULL,
	"name" text NOT NULL,
	"description" text,
	"sort_order" integer DEFAULT 0 NOT NULL,
	"is_active" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environments_key_unique" UNIQUE("key"),
	CONSTRAINT "environments_key_format" CHECK ("environments"."key" ~ '^[a-z][a-z0-9_]{0,62}$')
);
--> statement-breakpoint
CREATE TABLE "locations" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"key" text NOT NULL,
	"name" text NOT NULL,
	"description" text,
	"sort_order" integer DEFAULT 0 NOT NULL,
	"is_active" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"parent_id" uuid,
	"location_type" text NOT NULL,
	"address" text,
	CONSTRAINT "locations_key_unique" UNIQUE("key"),
	CONSTRAINT "locations_key_format" CHECK ("locations"."key" ~ '^[a-z][a-z0-9_]{0,62}$'),
	CONSTRAINT "locations_not_own_parent" CHECK ("locations"."parent_id" IS NULL OR "locations"."parent_id" <> "locations"."id"),
	CONSTRAINT "locations_type_valid" CHECK ("locations"."location_type" IN ('region', 'site', 'building', 'floor', 'room', 'rack', 'cloud_region', 'other'))
);
--> statement-breakpoint
CREATE TABLE "owners" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"kind" text NOT NULL,
	"name" text NOT NULL,
	"email" text,
	"external_ref" text,
	"is_active" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "owners_external_ref_unique" UNIQUE("external_ref"),
	CONSTRAINT "owners_kind_valid" CHECK ("owners"."kind" IN ('person', 'team')),
	CONSTRAINT "owners_email_format" CHECK ("owners"."email" IS NULL OR "owners"."email" ~ '^[^@\s]+@[^@\s]+$')
);
--> statement-breakpoint
CREATE TABLE "statuses" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"key" text NOT NULL,
	"name" text NOT NULL,
	"description" text,
	"sort_order" integer DEFAULT 0 NOT NULL,
	"is_active" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"is_operational" boolean DEFAULT false NOT NULL,
	CONSTRAINT "statuses_key_unique" UNIQUE("key"),
	CONSTRAINT "statuses_key_format" CHECK ("statuses"."key" ~ '^[a-z][a-z0-9_]{0,62}$')
);
--> statement-breakpoint
CREATE TABLE "ci_attribute_definitions" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"class_id" uuid NOT NULL,
	"key" text NOT NULL,
	"label" text NOT NULL,
	"description" text,
	"data_type" text NOT NULL,
	"is_required" boolean DEFAULT false NOT NULL,
	"enum_values" jsonb,
	"reference_class_id" uuid,
	"validation" jsonb,
	"group_name" text,
	"sort_order" integer DEFAULT 0 NOT NULL,
	"is_active" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "ci_attribute_definitions_class_key_uq" UNIQUE("class_id","key"),
	CONSTRAINT "ci_attribute_definitions_key_format" CHECK ("ci_attribute_definitions"."key" ~ '^[a-z][a-z0-9_]{0,62}$'),
	CONSTRAINT "ci_attribute_definitions_data_type_valid" CHECK ("ci_attribute_definitions"."data_type" IN ('text', 'number', 'integer', 'boolean', 'enum', 'date', 'datetime', 'ip', 'cidr', 'reference')),
	CONSTRAINT "ci_attribute_definitions_enum_values" CHECK (("ci_attribute_definitions"."data_type" = 'enum') = ("ci_attribute_definitions"."enum_values" IS NOT NULL)
          AND ("ci_attribute_definitions"."enum_values" IS NULL OR (jsonb_typeof("ci_attribute_definitions"."enum_values") = 'array' AND jsonb_array_length("ci_attribute_definitions"."enum_values") > 0))),
	CONSTRAINT "ci_attribute_definitions_reference_class" CHECK (("ci_attribute_definitions"."data_type" = 'reference') = ("ci_attribute_definitions"."reference_class_id" IS NOT NULL)),
	CONSTRAINT "ci_attribute_definitions_validation_object" CHECK ("ci_attribute_definitions"."validation" IS NULL OR jsonb_typeof("ci_attribute_definitions"."validation") = 'object')
);
--> statement-breakpoint
CREATE TABLE "ci_classes" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"key" text NOT NULL,
	"name" text NOT NULL,
	"description" text,
	"parent_id" uuid,
	"is_abstract" boolean DEFAULT false NOT NULL,
	"icon" text,
	"is_active" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "ci_classes_key_unique" UNIQUE("key"),
	CONSTRAINT "ci_classes_key_format" CHECK ("ci_classes"."key" ~ '^[a-z][a-z0-9_]{0,62}$'),
	CONSTRAINT "ci_classes_not_own_parent" CHECK ("ci_classes"."parent_id" IS NULL OR "ci_classes"."parent_id" <> "ci_classes"."id")
);
--> statement-breakpoint
CREATE TABLE "ci_attribute_values" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"ci_id" uuid NOT NULL,
	"attribute_id" uuid NOT NULL,
	"value_text" text,
	"value_number" numeric,
	"value_boolean" boolean,
	"value_date" date,
	"value_datetime" timestamp with time zone,
	"value_ip" "inet",
	"value_cidr" "cidr",
	"value_ref_ci_id" uuid,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "ci_attribute_values_ci_attribute_uq" UNIQUE("ci_id","attribute_id"),
	CONSTRAINT "ci_attribute_values_exactly_one_value" CHECK (num_nonnulls("ci_attribute_values"."value_text", "ci_attribute_values"."value_number", "ci_attribute_values"."value_boolean", "ci_attribute_values"."value_date",
                       "ci_attribute_values"."value_datetime", "ci_attribute_values"."value_ip", "ci_attribute_values"."value_cidr", "ci_attribute_values"."value_ref_ci_id") = 1)
);
--> statement-breakpoint
CREATE TABLE "configuration_items" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"class_id" uuid NOT NULL,
	"name" text NOT NULL,
	"status_id" uuid NOT NULL,
	"environment_id" uuid,
	"owner_id" uuid,
	"location_id" uuid,
	"hostname" text,
	"ip_address" "inet",
	"serial_number" text,
	"notes" text,
	"version" integer DEFAULT 1 NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"deleted_at" timestamp with time zone,
	"search_vector" "tsvector" GENERATED ALWAYS AS (to_tsvector('simple',
            coalesce(name, '') || ' ' || coalesce(hostname, '') || ' ' ||
            coalesce(serial_number, '') || ' ' || coalesce(notes, ''))) STORED,
	CONSTRAINT "configuration_items_name_not_blank" CHECK (length(btrim("configuration_items"."name")) > 0),
	CONSTRAINT "configuration_items_version_positive" CHECK ("configuration_items"."version" > 0),
	CONSTRAINT "configuration_items_hostname_format" CHECK ("configuration_items"."hostname" IS NULL OR "configuration_items"."hostname" ~ '^[A-Za-z0-9]([A-Za-z0-9._-]{0,252})$')
);
--> statement-breakpoint
CREATE TABLE "ci_relationships" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"relationship_type_id" uuid NOT NULL,
	"source_ci_id" uuid NOT NULL,
	"target_ci_id" uuid NOT NULL,
	"notes" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"deleted_at" timestamp with time zone,
	CONSTRAINT "ci_relationships_no_self_edge" CHECK ("ci_relationships"."source_ci_id" <> "ci_relationships"."target_ci_id")
);
--> statement-breakpoint
CREATE TABLE "relationship_type_rules" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"relationship_type_id" uuid NOT NULL,
	"source_class_id" uuid NOT NULL,
	"target_class_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "relationship_type_rules_uq" UNIQUE("relationship_type_id","source_class_id","target_class_id")
);
--> statement-breakpoint
CREATE TABLE "relationship_types" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"key" text NOT NULL,
	"name" text NOT NULL,
	"description" text,
	"sort_order" integer DEFAULT 0 NOT NULL,
	"is_active" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"forward_label" text NOT NULL,
	"reverse_label" text NOT NULL,
	"is_directional" boolean DEFAULT true NOT NULL,
	CONSTRAINT "relationship_types_key_unique" UNIQUE("key"),
	CONSTRAINT "relationship_types_key_format" CHECK ("relationship_types"."key" ~ '^[a-z][a-z0-9_]{0,62}$')
);
--> statement-breakpoint
CREATE TABLE "audit_log" (
	"id" bigint PRIMARY KEY GENERATED ALWAYS AS IDENTITY (sequence name "audit_log_id_seq" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 1),
	"occurred_at" timestamp with time zone DEFAULT now() NOT NULL,
	"actor_type" text NOT NULL,
	"actor_id" text,
	"actor_name" text,
	"action" text NOT NULL,
	"entity_type" text NOT NULL,
	"entity_id" uuid NOT NULL,
	"old_value" jsonb,
	"new_value" jsonb,
	"request_id" text,
	CONSTRAINT "audit_log_actor_type_valid" CHECK ("audit_log"."actor_type" IN ('system', 'user', 'api_client', 'import')),
	CONSTRAINT "audit_log_action_valid" CHECK ("audit_log"."action" IN ('create', 'update', 'delete', 'restore')),
	CONSTRAINT "audit_log_values_present" CHECK (("audit_log"."action" = 'create' AND "audit_log"."old_value" IS NULL AND "audit_log"."new_value" IS NOT NULL)
          OR ("audit_log"."action" = 'update' AND "audit_log"."old_value" IS NOT NULL AND "audit_log"."new_value" IS NOT NULL)
          OR ("audit_log"."action" IN ('delete', 'restore') AND "audit_log"."old_value" IS NOT NULL))
);
--> statement-breakpoint
ALTER TABLE "locations" ADD CONSTRAINT "locations_parent_id_locations_id_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."locations"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_attribute_definitions" ADD CONSTRAINT "ci_attribute_definitions_class_id_ci_classes_id_fk" FOREIGN KEY ("class_id") REFERENCES "public"."ci_classes"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_attribute_definitions" ADD CONSTRAINT "ci_attribute_definitions_reference_class_id_ci_classes_id_fk" FOREIGN KEY ("reference_class_id") REFERENCES "public"."ci_classes"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_classes" ADD CONSTRAINT "ci_classes_parent_id_ci_classes_id_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."ci_classes"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_attribute_values" ADD CONSTRAINT "ci_attribute_values_ci_id_configuration_items_id_fk" FOREIGN KEY ("ci_id") REFERENCES "public"."configuration_items"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_attribute_values" ADD CONSTRAINT "ci_attribute_values_attribute_id_ci_attribute_definitions_id_fk" FOREIGN KEY ("attribute_id") REFERENCES "public"."ci_attribute_definitions"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_attribute_values" ADD CONSTRAINT "ci_attribute_values_value_ref_ci_id_configuration_items_id_fk" FOREIGN KEY ("value_ref_ci_id") REFERENCES "public"."configuration_items"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "configuration_items" ADD CONSTRAINT "configuration_items_class_id_ci_classes_id_fk" FOREIGN KEY ("class_id") REFERENCES "public"."ci_classes"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "configuration_items" ADD CONSTRAINT "configuration_items_status_id_statuses_id_fk" FOREIGN KEY ("status_id") REFERENCES "public"."statuses"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "configuration_items" ADD CONSTRAINT "configuration_items_environment_id_environments_id_fk" FOREIGN KEY ("environment_id") REFERENCES "public"."environments"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "configuration_items" ADD CONSTRAINT "configuration_items_owner_id_owners_id_fk" FOREIGN KEY ("owner_id") REFERENCES "public"."owners"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "configuration_items" ADD CONSTRAINT "configuration_items_location_id_locations_id_fk" FOREIGN KEY ("location_id") REFERENCES "public"."locations"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_relationships" ADD CONSTRAINT "ci_relationships_relationship_type_id_relationship_types_id_fk" FOREIGN KEY ("relationship_type_id") REFERENCES "public"."relationship_types"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_relationships" ADD CONSTRAINT "ci_relationships_source_ci_id_configuration_items_id_fk" FOREIGN KEY ("source_ci_id") REFERENCES "public"."configuration_items"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "ci_relationships" ADD CONSTRAINT "ci_relationships_target_ci_id_configuration_items_id_fk" FOREIGN KEY ("target_ci_id") REFERENCES "public"."configuration_items"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "relationship_type_rules" ADD CONSTRAINT "relationship_type_rules_relationship_type_id_relationship_types_id_fk" FOREIGN KEY ("relationship_type_id") REFERENCES "public"."relationship_types"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "relationship_type_rules" ADD CONSTRAINT "relationship_type_rules_source_class_id_ci_classes_id_fk" FOREIGN KEY ("source_class_id") REFERENCES "public"."ci_classes"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "relationship_type_rules" ADD CONSTRAINT "relationship_type_rules_target_class_id_ci_classes_id_fk" FOREIGN KEY ("target_class_id") REFERENCES "public"."ci_classes"("id") ON DELETE restrict ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "locations_parent_idx" ON "locations" USING btree ("parent_id");--> statement-breakpoint
CREATE INDEX "owners_name_idx" ON "owners" USING btree (lower("name"));--> statement-breakpoint
CREATE INDEX "ci_attribute_definitions_reference_class_idx" ON "ci_attribute_definitions" USING btree ("reference_class_id");--> statement-breakpoint
CREATE INDEX "ci_classes_parent_idx" ON "ci_classes" USING btree ("parent_id");--> statement-breakpoint
CREATE INDEX "ci_attribute_values_text_idx" ON "ci_attribute_values" USING btree ("attribute_id","value_text") WHERE "ci_attribute_values"."value_text" IS NOT NULL;--> statement-breakpoint
CREATE INDEX "ci_attribute_values_number_idx" ON "ci_attribute_values" USING btree ("attribute_id","value_number") WHERE "ci_attribute_values"."value_number" IS NOT NULL;--> statement-breakpoint
CREATE INDEX "ci_attribute_values_ref_idx" ON "ci_attribute_values" USING btree ("value_ref_ci_id") WHERE "ci_attribute_values"."value_ref_ci_id" IS NOT NULL;--> statement-breakpoint
CREATE INDEX "ci_attribute_values_attribute_idx" ON "ci_attribute_values" USING btree ("attribute_id");--> statement-breakpoint
CREATE INDEX "configuration_items_live_name_idx" ON "configuration_items" USING btree (lower("name"),"id") WHERE "configuration_items"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "configuration_items_live_updated_idx" ON "configuration_items" USING btree ("updated_at" DESC NULLS LAST,"id") WHERE "configuration_items"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "configuration_items_class_idx" ON "configuration_items" USING btree ("class_id",lower("name")) WHERE "configuration_items"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "configuration_items_status_idx" ON "configuration_items" USING btree ("status_id") WHERE "configuration_items"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "configuration_items_owner_idx" ON "configuration_items" USING btree ("owner_id") WHERE "configuration_items"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "configuration_items_location_idx" ON "configuration_items" USING btree ("location_id") WHERE "configuration_items"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "configuration_items_environment_idx" ON "configuration_items" USING btree ("environment_id") WHERE "configuration_items"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "configuration_items_search_idx" ON "configuration_items" USING gin ("search_vector");--> statement-breakpoint
CREATE INDEX "configuration_items_name_trgm_idx" ON "configuration_items" USING gin ("name" gin_trgm_ops);--> statement-breakpoint
CREATE INDEX "configuration_items_hostname_trgm_idx" ON "configuration_items" USING gin ("hostname" gin_trgm_ops);--> statement-breakpoint
CREATE INDEX "configuration_items_serial_trgm_idx" ON "configuration_items" USING gin ("serial_number" gin_trgm_ops);--> statement-breakpoint
CREATE INDEX "configuration_items_ip_idx" ON "configuration_items" USING gist ("ip_address" inet_ops);--> statement-breakpoint
CREATE UNIQUE INDEX "ci_relationships_live_edge_uq" ON "ci_relationships" USING btree ("relationship_type_id","source_ci_id","target_ci_id") WHERE "ci_relationships"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "ci_relationships_source_idx" ON "ci_relationships" USING btree ("source_ci_id","relationship_type_id") WHERE "ci_relationships"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "ci_relationships_target_idx" ON "ci_relationships" USING btree ("target_ci_id","relationship_type_id") WHERE "ci_relationships"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "relationship_type_rules_source_idx" ON "relationship_type_rules" USING btree ("source_class_id");--> statement-breakpoint
CREATE INDEX "relationship_type_rules_target_idx" ON "relationship_type_rules" USING btree ("target_class_id");--> statement-breakpoint
CREATE INDEX "audit_log_entity_idx" ON "audit_log" USING btree ("entity_type","entity_id","occurred_at" DESC NULLS LAST);--> statement-breakpoint
CREATE INDEX "audit_log_occurred_idx" ON "audit_log" USING btree ("occurred_at" DESC NULLS LAST);--> statement-breakpoint
CREATE INDEX "audit_log_actor_idx" ON "audit_log" USING btree ("actor_id","occurred_at" DESC NULLS LAST) WHERE "audit_log"."actor_id" IS NOT NULL;