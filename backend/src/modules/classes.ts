import { eq, inArray, isNull, sql } from 'drizzle-orm';
import { z } from 'zod';
import type { Database } from '../db/client.js';
import {
  ATTRIBUTE_DATA_TYPES,
  ciAttributeDefinitions,
  ciClasses,
  relationshipTypeRules,
  relationshipTypes,
} from '../db/schema/index.js';
import {
  attributeKeyClash,
  classHasItems,
  effectiveAttributes,
  enumValuesInUse,
  orphanedAttributeValues,
} from '../data/classes.js';
import { serialise } from '../data/crud.js';
import { AppError } from '../http/errors.js';
import { defineRoute, type RouteSpec } from '../http/route.js';
import {
  Description,
  IdParams,
  Key,
  Name,
  QueryBool,
  QueryUuidList,
  Timestamp,
  Uuid,
  component,
} from '../http/schemas.js';
import {
  SimpleResourceService,
  activeFilter,
  nonEmptyPatch,
  simpleResourceRoutes,
  type SimpleResourceConfig,
} from './simple-resource.js';

// ===========================================================================
// CI classes
// ===========================================================================

const CiClass = component(
  'CiClass',
  z.object({
    id: Uuid,
    key: z.string(),
    name: z.string(),
    description: z.string().nullable(),
    parentId: Uuid.nullable().describe('Parent class; attributes and relationship rules are inherited from it'),
    isAbstract: z.boolean().describe('Abstract classes group attributes and rules but cannot hold CIs'),
    icon: z.string().nullable(),
    isActive: z.boolean(),
    createdAt: Timestamp,
    updatedAt: Timestamp,
  }),
);

const classWritable = {
  name: Name,
  description: Description.nullable().optional(),
  parentId: Uuid.nullable().optional(),
  isAbstract: z.boolean().optional(),
  icon: z.string().max(100).nullable().optional(),
  isActive: z.boolean().optional(),
};
const classSortFields = ['name', 'key', 'createdAt', 'updatedAt'] as const;

const classConfig: SimpleResourceConfig<typeof classSortFields> = {
  table: ciClasses,
  entityType: 'ci_classes',
  label: 'CI class',
  basePath: '/api/v1/ci-classes',
  tag: 'CI classes',
  names: { singular: 'ciClass', plural: 'ciClasses' },
  dto: CiClass,
  createBody: z.strictObject({ key: Key, ...classWritable }),
  // `key` is immutable: imports, integrations and reports refer to it.
  updateBody: nonEmptyPatch(z.strictObject(classWritable).partial()),
  filterShape: {
    isActive: QueryBool.optional(),
    isAbstract: QueryBool.optional(),
    parentId: z.union([z.literal('none'), Uuid]).optional().describe('Direct children of this class; "none" for root classes'),
    descendantOf: Uuid.optional().describe('This class and every class below it'),
  },
  filters: (q) => [
    activeFilter(ciClasses.isActive)(q.isActive),
    activeFilter(ciClasses.isAbstract)(q.isAbstract),
    q.parentId === 'none' ? isNull(ciClasses.parentId) : q.parentId ? eq(ciClasses.parentId, q.parentId as string) : undefined,
    q.descendantOf ? sql`ci_class_is_a(${ciClasses.id}, ${q.descendantOf as string})` : undefined,
  ],
  searchColumns: [ciClasses.key, ciClasses.name, ciClasses.description],
  sortFields: classSortFields,
  sortColumns: { name: ciClasses.name, key: ciClasses.key, createdAt: ciClasses.createdAt, updatedAt: ciClasses.updatedAt },
  defaultSort: 'name',
  afterWrite: async (tx, row, previous) => {
    if (previous && row.isAbstract && !previous.isAbstract && (await classHasItems(tx, row.id))) {
      throw AppError.field('isAbstract', 'Class still holds CIs; an abstract class cannot', 'class_has_items');
    }
    if (previous && row.parentId !== previous.parentId) {
      const orphaned = await orphanedAttributeValues(tx, row.id);
      if (orphaned.length) {
        throw AppError.field(
          'parentId',
          `CIs of this class hold values for attributes that the new parent does not provide: ${orphaned.join(', ')}`,
          'attributes_outside_lineage',
        );
      }
    }
  },
};

// ===========================================================================
// Attribute definitions
// ===========================================================================

const ValidationRules = z
  .strictObject({
    min: z.number().optional().describe('number/integer: minimum'),
    max: z.number().optional().describe('number/integer: maximum'),
    maxLength: z.number().int().min(1).optional().describe('text: maximum length'),
    pattern: z.string().max(500).optional().describe('text: regular expression the value must match'),
    unit: z.string().max(20).optional().describe('Display unit, e.g. "GB"'),
  })
  .describe('Extra validation the API enforces on attribute values');

const AttributeDefinition = component(
  'AttributeDefinition',
  z.object({
    id: Uuid,
    classId: Uuid,
    key: z.string(),
    label: z.string(),
    description: z.string().nullable(),
    dataType: z.enum(ATTRIBUTE_DATA_TYPES),
    isRequired: z.boolean(),
    enumValues: z.array(z.string()).nullable().describe('Allowed values when dataType is "enum"'),
    referenceClassId: Uuid.nullable().describe('When dataType is "reference": the class (or ancestor) the referenced CI must belong to'),
    validation: z.record(z.string(), z.unknown()).nullable(),
    groupName: z.string().nullable().describe('UI grouping, e.g. "Hardware"'),
    sortOrder: z.number().int(),
    isActive: z.boolean(),
    createdAt: Timestamp,
    updatedAt: Timestamp,
  }),
);

const EffectiveAttribute = component(
  'EffectiveAttribute',
  AttributeDefinition.extend({
    inherited: z.boolean().describe('Defined on an ancestor class rather than this one'),
    definedOn: z.object({ id: Uuid, key: z.string(), name: z.string() }),
  }),
);
const EffectiveAttributeList = component(
  'EffectiveAttributeList',
  z.object({ data: z.array(EffectiveAttribute) }).describe('All attributes a CI of this class can carry (not paginated; bounded by the class lineage)'),
);

const EnumValues = z
  .array(z.string().trim().min(1).max(200))
  .min(1)
  .max(500)
  .refine((v) => new Set(v).size === v.length, 'Values must be unique');

const attrMutable = {
  label: Name,
  description: Description.nullable().optional(),
  isRequired: z.boolean().optional(),
  enumValues: EnumValues.nullable().optional(),
  validation: ValidationRules.nullable().optional(),
  groupName: z.string().trim().max(100).nullable().optional(),
  sortOrder: z.number().int().min(-1_000_000).max(1_000_000).optional(),
  isActive: z.boolean().optional(),
};

function checkValidationRules(
  v: z.infer<typeof ValidationRules> | null | undefined,
  dataType: string,
  addIssue: (field: string, message: string) => void,
) {
  if (!v) return;
  const numeric = dataType === 'number' || dataType === 'integer';
  if ((v.min !== undefined || v.max !== undefined) && !numeric) addIssue('validation', 'min/max apply to number and integer attributes only');
  if ((v.pattern !== undefined || v.maxLength !== undefined) && dataType !== 'text')
    addIssue('validation', 'pattern/maxLength apply to text attributes only');
  if (v.min !== undefined && v.max !== undefined && v.min > v.max) addIssue('validation.min', 'min must not exceed max');
  if (v.pattern !== undefined) {
    try {
      new RegExp(v.pattern);
    } catch {
      addIssue('validation.pattern', 'Not a valid regular expression');
    }
  }
}

const attrCreate = z
  .strictObject({
    classId: Uuid,
    key: Key,
    dataType: z.enum(ATTRIBUTE_DATA_TYPES),
    referenceClassId: Uuid.nullable().optional(),
    ...attrMutable,
  })
  .superRefine((b, ctx) => {
    const add = (field: string, message: string) => ctx.addIssue({ code: 'custom', path: field.split('.'), message });
    if (b.dataType === 'enum' && !b.enumValues) add('enumValues', 'Required for enum attributes');
    if (b.dataType !== 'enum' && b.enumValues) add('enumValues', 'Only allowed for enum attributes');
    if (b.dataType === 'reference' && !b.referenceClassId) add('referenceClassId', 'Required for reference attributes');
    if (b.dataType !== 'reference' && b.referenceClassId) add('referenceClassId', 'Only allowed for reference attributes');
    checkValidationRules(b.validation, b.dataType, add);
  });

const attrSortFields = ['sortOrder', 'key', 'label', 'createdAt', 'updatedAt'] as const;

const attributeConfig: SimpleResourceConfig<typeof attrSortFields> = {
  table: ciAttributeDefinitions,
  entityType: 'ci_attribute_definitions',
  label: 'Attribute definition',
  basePath: '/api/v1/attribute-definitions',
  tag: 'Attribute definitions',
  names: { singular: 'attributeDefinition', plural: 'attributeDefinitions' },
  dto: AttributeDefinition,
  createBody: attrCreate,
  // classId, key and dataType are immutable: stored values depend on them.
  updateBody: nonEmptyPatch(z.strictObject(attrMutable).partial()),
  filterShape: {
    classId: QueryUuidList.optional().describe('Defined directly on these classes'),
    effectiveForClassId: Uuid.optional().describe('Everything a CI of this class can carry, including inherited definitions'),
    dataType: z.enum(ATTRIBUTE_DATA_TYPES).optional(),
    isActive: QueryBool.optional(),
    isRequired: QueryBool.optional(),
  },
  filters: (q) => [
    q.classId ? inArray(ciAttributeDefinitions.classId, q.classId as string[]) : undefined,
    q.effectiveForClassId ? sql`ci_class_is_a(${q.effectiveForClassId as string}, ${ciAttributeDefinitions.classId})` : undefined,
    q.dataType ? eq(ciAttributeDefinitions.dataType, q.dataType as (typeof ATTRIBUTE_DATA_TYPES)[number]) : undefined,
    activeFilter(ciAttributeDefinitions.isActive)(q.isActive),
    activeFilter(ciAttributeDefinitions.isRequired)(q.isRequired),
  ],
  searchColumns: [ciAttributeDefinitions.key, ciAttributeDefinitions.label, ciAttributeDefinitions.description, ciAttributeDefinitions.groupName],
  sortFields: attrSortFields,
  sortColumns: {
    sortOrder: ciAttributeDefinitions.sortOrder,
    key: ciAttributeDefinitions.key,
    label: ciAttributeDefinitions.label,
    createdAt: ciAttributeDefinitions.createdAt,
    updatedAt: ciAttributeDefinitions.updatedAt,
  },
  defaultSort: 'sortOrder',
  afterWrite: async (tx, row, previous) => {
    const r = row as unknown as typeof ciAttributeDefinitions.$inferSelect;
    const clash = await attributeKeyClash(tx, r.classId, r.key, r.id);
    if (clash) throw new AppError('CONFLICT', `Attribute "${r.key}" is already defined on class "${clash}" in the same lineage`, [
      { in: 'body', field: 'key', message: 'Already defined on an ancestor or descendant class', code: 'unique' },
    ]);
    if (!previous) return;
    const issues: { field: string; message: string }[] = [];
    checkValidationRules(r.validation as never, r.dataType, (field, message) => issues.push({ field, message }));
    if (issues.length) throw AppError.validation(issues.map((i) => ({ in: 'body', ...i, code: 'invalid' })));
    if (r.dataType === 'enum' && r.enumValues) {
      const stale = await enumValuesInUse(tx, r.id, r.enumValues);
      if (stale.length) {
        throw AppError.field('enumValues', `Values still stored on CIs cannot be removed: ${stale.join(', ')}`, 'enum_value_in_use');
      }
    }
  },
};

// ===========================================================================
// Relationship types and rules
// ===========================================================================

const RelationshipType = component(
  'RelationshipType',
  z.object({
    id: Uuid,
    key: z.string(),
    name: z.string(),
    description: z.string().nullable(),
    forwardLabel: z.string().describe('Reads source -> target, e.g. "runs on"'),
    reverseLabel: z.string().describe('Reads target -> source, e.g. "hosts"'),
    isDirectional: z.boolean().describe('false for symmetric types such as connected_to'),
    sortOrder: z.number().int(),
    isActive: z.boolean(),
    createdAt: Timestamp,
    updatedAt: Timestamp,
  }),
);
const relTypeMutable = {
  name: Name,
  description: Description.nullable().optional(),
  forwardLabel: Name,
  reverseLabel: Name,
  sortOrder: z.number().int().min(-1_000_000).max(1_000_000).optional(),
  isActive: z.boolean().optional(),
};
const relTypeSortFields = ['sortOrder', 'name', 'key', 'createdAt', 'updatedAt'] as const;

const relationshipTypeConfig: SimpleResourceConfig<typeof relTypeSortFields> = {
  table: relationshipTypes,
  entityType: 'relationship_types',
  label: 'Relationship type',
  basePath: '/api/v1/relationship-types',
  tag: 'Relationship types',
  names: { singular: 'relationshipType', plural: 'relationshipTypes' },
  dto: RelationshipType,
  createBody: z.strictObject({ key: Key, isDirectional: z.boolean().optional(), ...relTypeMutable }),
  // key and isDirectional are immutable: existing edges were validated against them.
  updateBody: nonEmptyPatch(z.strictObject(relTypeMutable).partial()),
  filterShape: {
    isActive: QueryBool.optional(),
    sourceClassId: Uuid.optional().describe('Only types a CI of this class may use as source (rules are inherited)'),
    targetClassId: Uuid.optional().describe('Only types a CI of this class may use as target; combine with sourceClassId for a pair'),
  },
  filters: (q) => {
    const src = q.sourceClassId as string | undefined;
    const tgt = q.targetClassId as string | undefined;
    const pair = (a: string | undefined, b: string | undefined) =>
      sql`(${a ? sql`ci_class_is_a(${a}, r.source_class_id)` : sql`true`} AND ${b ? sql`ci_class_is_a(${b}, r.target_class_id)` : sql`true`})`;
    return [
      activeFilter(relationshipTypes.isActive)(q.isActive),
      src || tgt
        ? sql`EXISTS (SELECT 1 FROM relationship_type_rules r WHERE r.relationship_type_id = ${relationshipTypes.id}
                AND (${pair(src, tgt)} OR (NOT ${relationshipTypes.isDirectional} AND ${pair(tgt, src)})))`
        : undefined,
    ];
  },
  searchColumns: [relationshipTypes.key, relationshipTypes.name, relationshipTypes.forwardLabel, relationshipTypes.reverseLabel],
  sortFields: relTypeSortFields,
  sortColumns: {
    sortOrder: relationshipTypes.sortOrder,
    name: relationshipTypes.name,
    key: relationshipTypes.key,
    createdAt: relationshipTypes.createdAt,
    updatedAt: relationshipTypes.updatedAt,
  },
  defaultSort: 'sortOrder',
};

const RelationshipRule = component(
  'RelationshipRule',
  z.object({
    id: Uuid,
    relationshipTypeId: Uuid,
    sourceClassId: Uuid.describe('Matches this class and all its descendants'),
    targetClassId: Uuid.describe('Matches this class and all its descendants'),
    createdAt: Timestamp,
    updatedAt: Timestamp,
  }),
);
const ruleWritable = { relationshipTypeId: Uuid, sourceClassId: Uuid, targetClassId: Uuid };
const ruleSortFields = ['createdAt', 'updatedAt'] as const;

const relationshipRuleConfig: SimpleResourceConfig<typeof ruleSortFields> = {
  table: relationshipTypeRules,
  entityType: 'relationship_type_rules',
  label: 'Relationship rule',
  basePath: '/api/v1/relationship-rules',
  tag: 'Relationship types',
  names: { singular: 'relationshipRule', plural: 'relationshipRules' },
  dto: RelationshipRule,
  createBody: z.strictObject(ruleWritable),
  updateBody: nonEmptyPatch(z.strictObject(ruleWritable).partial()),
  filterShape: {
    relationshipTypeId: QueryUuidList.optional(),
    sourceClassId: Uuid.optional().describe('Rules on this class or an ancestor (i.e. rules that apply to it)'),
    targetClassId: Uuid.optional().describe('Rules on this class or an ancestor (i.e. rules that apply to it)'),
  },
  filters: (q) => [
    q.relationshipTypeId ? inArray(relationshipTypeRules.relationshipTypeId, q.relationshipTypeId as string[]) : undefined,
    q.sourceClassId ? sql`ci_class_is_a(${q.sourceClassId as string}, ${relationshipTypeRules.sourceClassId})` : undefined,
    q.targetClassId ? sql`ci_class_is_a(${q.targetClassId as string}, ${relationshipTypeRules.targetClassId})` : undefined,
  ],
  searchColumns: [],
  sortFields: ruleSortFields,
  sortColumns: { createdAt: relationshipTypeRules.createdAt, updatedAt: relationshipTypeRules.updatedAt },
  defaultSort: 'createdAt',
  deleteDescription: 'Hard delete. Existing relationships stay; new ones need another matching rule.',
};

// ===========================================================================

export function classRoutes(db: Database): RouteSpec[] {
  const cfgs = [classConfig, attributeConfig, relationshipTypeConfig, relationshipRuleConfig] as SimpleResourceConfig<
    readonly [string, ...string[]]
  >[];
  const [classes, ...rest] = cfgs.map((cfg) => simpleResourceRoutes(new SimpleResourceService(db, cfg), cfg));

  const effective = defineRoute({
    method: 'GET',
    url: '/api/v1/ci-classes/:id/attributes',
    operationId: 'listCiClassEffectiveAttributes',
    tag: 'CI classes',
    summary: 'Every attribute a CI of this class can carry, including inherited ones',
    description: 'Ordered root class first, then by sortOrder. Use it to render the CI form for a class.',
    params: IdParams,
    query: z.strictObject({ includeInactive: QueryBool.optional().describe('Include retired definitions (default false)') }),
    response: EffectiveAttributeList,
    errors: ['NOT_FOUND'],
    handler: async ({ params, query }) => {
      const [cls] = await db.select({ id: ciClasses.id }).from(ciClasses).where(eq(ciClasses.id, params.id));
      if (!cls) throw AppError.notFound('CI class', params.id);
      const rows = await effectiveAttributes(db, params.id);
      return {
        data: rows
          .filter((r) => query.includeInactive || r.is_active)
          .map((r) => ({
            ...attributeRowToDto(r),
            inherited: r.depth > 0,
            definedOn: { id: r.class_id, key: r.defined_on_key, name: r.defined_on_name },
          })),
      };
    },
  });

  return [...classes!, effective, ...rest.flat()];
}

/** Raw (snake_case) attribute-definition row -> API shape. */
export function attributeRowToDto(r: {
  id: string;
  class_id: string;
  key: string;
  label: string;
  description: string | null;
  data_type: string;
  is_required: boolean;
  enum_values: string[] | null;
  reference_class_id: string | null;
  validation: Record<string, unknown> | null;
  group_name: string | null;
  sort_order: number;
  is_active: boolean;
  created_at: Date | string;
  updated_at: Date | string;
}): z.output<typeof AttributeDefinition> {
  return serialise({
    id: r.id,
    classId: r.class_id,
    key: r.key,
    label: r.label,
    description: r.description,
    dataType: r.data_type,
    isRequired: r.is_required,
    enumValues: r.enum_values,
    referenceClassId: r.reference_class_id,
    validation: r.validation,
    groupName: r.group_name,
    sortOrder: r.sort_order,
    isActive: r.is_active,
    createdAt: new Date(r.created_at),
    updatedAt: new Date(r.updated_at),
  }) as z.output<typeof AttributeDefinition>;
}
