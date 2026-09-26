-- Extensions required by the schema. pg_trgm powers substring/fuzzy global
-- search; it ships with PostgreSQL contrib and is on the allow-list of the
-- major managed services (RDS, Cloud SQL, Azure Flexible Server).
CREATE EXTENSION IF NOT EXISTS pg_trgm;
