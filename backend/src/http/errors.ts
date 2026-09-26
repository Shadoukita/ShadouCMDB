import { z } from 'zod';
import { component } from './schemas.js';

/**
 * Every failure leaves the API in one envelope:
 *
 *   { "error": { "code": "VALIDATION_ERROR", "message": "...", "details": [...], "requestId": "..." } }
 *
 * `code` is machine-readable and stable; `message` is for humans; `details`
 * carries per-field problems (`in` says where the field lives).
 */
export const ERROR_CODES = [
  'VALIDATION_ERROR',
  'NOT_FOUND',
  'CONFLICT',
  'IN_USE',
  'VERSION_CONFLICT',
  'UNSUPPORTED_MEDIA_TYPE',
  'PAYLOAD_TOO_LARGE',
  'DATABASE_UNAVAILABLE',
  'INTERNAL_ERROR',
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number];

export const FIELD_LOCATIONS = ['body', 'query', 'params', 'header'] as const;
export type FieldLocation = (typeof FIELD_LOCATIONS)[number];

export interface FieldError {
  in: FieldLocation;
  field: string;
  message: string;
  code: string;
}

export const ErrorEnvelopeSchema = component(
  'ErrorEnvelope',
  z.object({
    error: z.object({
      code: z.enum(ERROR_CODES),
      message: z.string(),
      details: z
        .array(
          z.object({
            in: z.enum(FIELD_LOCATIONS),
            field: z.string().describe('Dotted path, e.g. "attributes.cpu_cores" or "limit"'),
            message: z.string(),
            code: z.string().describe('Machine-readable reason, e.g. "invalid_type", "required", "unique"'),
          }),
        )
        .optional(),
      requestId: z.string(),
    }),
  }),
);

const STATUS_BY_CODE: Record<ErrorCode, number> = {
  VALIDATION_ERROR: 400,
  NOT_FOUND: 404,
  CONFLICT: 409,
  IN_USE: 409,
  VERSION_CONFLICT: 409,
  UNSUPPORTED_MEDIA_TYPE: 415,
  PAYLOAD_TOO_LARGE: 413,
  DATABASE_UNAVAILABLE: 503,
  INTERNAL_ERROR: 500,
};

export class AppError extends Error {
  readonly statusCode: number;
  constructor(
    readonly code: ErrorCode,
    message: string,
    readonly details?: FieldError[],
  ) {
    super(message);
    this.statusCode = STATUS_BY_CODE[code];
  }

  static validation(details: FieldError[], message = 'Request validation failed'): AppError {
    return new AppError('VALIDATION_ERROR', message, details);
  }

  static field(field: string, message: string, code = 'invalid', where: FieldLocation = 'body'): AppError {
    return AppError.validation([{ in: where, field, message, code }], message);
  }

  static notFound(entity: string, id: string): AppError {
    return new AppError('NOT_FOUND', `${entity} ${id} not found`);
  }
}

/** Convert zod issues to field errors at the given request location. */
export function zodToFieldErrors(error: z.ZodError, where: FieldLocation, prefix = ''): FieldError[] {
  return error.issues.map((issue) => {
    const path = issue.path.map(String).join('.');
    const missing = issue.code === 'invalid_type' && issue.input === undefined;
    return {
      in: where,
      field: [prefix, path].filter(Boolean).join('.') || '(root)',
      message: missing ? 'Required' : issue.message,
      code: missing ? 'required' : issue.code,
    };
  });
}

// ---------------------------------------------------------------------------
// PostgreSQL error mapping
// ---------------------------------------------------------------------------

interface PgError {
  code: string;
  message: string;
  detail?: string;
  constraint?: string;
  column?: string;
  table?: string;
}

/** Drizzle wraps driver errors; walk the cause chain to the pg error, if any. */
export function findPgError(err: unknown): PgError | undefined {
  let cur: unknown = err;
  for (let i = 0; i < 5 && cur; i++) {
    if (typeof cur === 'object' && cur !== null && 'code' in cur && 'severity' in cur) return cur as unknown as PgError;
    cur = (cur as { cause?: unknown }).cause;
  }
  return undefined;
}

const CONNECTION_ERROR_CODES = new Set(['ECONNREFUSED', 'ENOTFOUND', 'ETIMEDOUT', 'ECONNRESET', 'EHOSTUNREACH', 'EAI_AGAIN']);

export function isConnectionError(err: unknown): boolean {
  let cur: unknown = err;
  for (let i = 0; i < 5 && cur; i++) {
    const code = (cur as { code?: unknown }).code;
    if (typeof code === 'string' && (CONNECTION_ERROR_CODES.has(code) || code.startsWith('08') || code === '57P01'))
      return true;
    const msg = (cur as { message?: unknown }).message;
    if (typeof msg === 'string' && /Connection terminated|timeout exceeded when trying to connect/i.test(msg)) return true;
    cur = (cur as { cause?: unknown }).cause;
  }
  return false;
}

/** Constraints (declarative and trigger-raised) mapped to the API field they concern. */
const CONSTRAINT_FIELDS: Record<string, string> = {
  ci_classes_not_own_parent: 'parentId',
  ci_classes_no_cycle: 'parentId',
  locations_not_own_parent: 'parentId',
  locations_no_cycle: 'parentId',
  locations_type_valid: 'locationType',
  owners_email_format: 'email',
  owners_kind_valid: 'kind',
  owners_external_ref_unique: 'externalRef',
  ci_attribute_definitions_class_key_uq: 'key',
  ci_attribute_definitions_enum_values: 'enumValues',
  ci_attribute_definitions_reference_class: 'referenceClassId',
  ci_attribute_definitions_validation_object: 'validation',
  ci_attribute_definitions_data_type_valid: 'dataType',
  configuration_items_name_not_blank: 'name',
  configuration_items_hostname_format: 'hostname',
  configuration_items_class_concrete: 'classId',
  configuration_items_class_active: 'classId',
  configuration_items_class_change_attributes: 'classId',
  ci_relationships_no_self_edge: 'targetCiId',
  ci_relationships_endpoint_rule: 'relationshipTypeId',
  ci_relationships_type_active: 'relationshipTypeId',
  ci_relationships_live_endpoints: 'sourceCiId',
  ci_relationships_live_edge_uq: 'targetCiId',
  relationship_type_rules_uq: 'targetClassId',
};

const snakeToCamel = (s: string) => s.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());

function fieldFor(pg: PgError): string {
  if (pg.constraint && CONSTRAINT_FIELDS[pg.constraint]) return CONSTRAINT_FIELDS[pg.constraint]!;
  if (pg.constraint && /_key_(format|unique)$/.test(pg.constraint)) return 'key';
  if (pg.column) return snakeToCamel(pg.column);
  // "Key (status_id)=(...) is not present in table ..." / "Key (key)=(x) already exists."
  const m = pg.detail?.match(/^Key \(([a-z_]+)\)/);
  if (m?.[1]) return snakeToCamel(m[1]);
  return '(root)';
}

/** Trigger messages are "table: human text"; drop the table prefix. */
const humanise = (msg: string) => msg.replace(/^[a-z_]+: /, '');

/**
 * Translate a PostgreSQL error into an AppError, or return undefined when it is
 * not a client-caused error. `fieldPrefix` lets callers scope errors raised
 * while writing a nested value (e.g. "attributes.cpu_cores").
 */
export function mapPgError(err: unknown, fieldPrefix?: string): AppError | undefined {
  const pg = findPgError(err);
  if (!pg) return undefined;
  const field = fieldPrefix ?? fieldFor(pg);
  switch (pg.code) {
    case '23505':
      return new AppError('CONFLICT', humanise(pg.detail ?? pg.message), [
        { in: 'body', field, message: 'Already exists', code: 'unique' },
      ]);
    case '23001': // restrict_violation (ON DELETE RESTRICT)
    case '23503':
      if (pg.code === '23001' || /is still referenced/.test(pg.detail ?? '')) {
        return new AppError(
          'IN_USE',
          'This record is still referenced by other records. Retire it with isActive=false instead of deleting it.',
        );
      }
      return AppError.field(field, 'Referenced record does not exist', 'not_found');
    case '23514':
      return AppError.field(field, humanise(pg.message), pg.constraint ?? 'check_violation');
    case '23502':
      return AppError.field(field, 'Required', 'required');
    case '22P02': // invalid_text_representation (bad inet, uuid, ...)
    case '22007': // invalid_datetime_format
    case '22008': // datetime_field_overflow
    case '22003': // numeric_value_out_of_range
    case '22001': // string_data_right_truncation
      return AppError.field(field, pg.message, 'invalid_format');
    default:
      return undefined;
  }
}
