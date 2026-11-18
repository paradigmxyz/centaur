-- A deterministic text description of the console schema in `public`, used to
-- compare a Rails-migrated database with a fresh SQLx install without needing
-- pg_dump. SQLx's own bookkeeping table is excluded. One line per object.
WITH lines AS (
    SELECT 'table ' || c.relname AS line
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') AND c.relname <> '_sqlx_migrations'
  UNION ALL
    SELECT 'column ' || c.relname || '.' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod)
        || CASE WHEN a.attnotnull THEN ' not null' ELSE '' END
        || CASE WHEN a.attgenerated = 's' THEN ' generated ' || pg_get_expr(d.adbin, d.adrelid)
                WHEN d.adbin IS NOT NULL THEN ' default ' || pg_get_expr(d.adbin, d.adrelid)
                ELSE '' END
        || CASE WHEN a.attidentity <> '' THEN ' identity ' || a.attidentity::text ELSE '' END
        || ' #' || a.attnum
    FROM pg_attribute a
    JOIN pg_class c ON c.oid = a.attrelid
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
    WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') AND c.relname <> '_sqlx_migrations'
      AND a.attnum > 0 AND NOT a.attisdropped
  UNION ALL
    SELECT 'constraint ' || c.relname || '.' || con.conname || ' ' || pg_get_constraintdef(con.oid, true)
    FROM pg_constraint con
    JOIN pg_class c ON c.oid = con.conrelid
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND c.relname <> '_sqlx_migrations'
  UNION ALL
    SELECT 'index ' || i.indexname || ' ' || i.indexdef
    FROM pg_indexes i
    WHERE i.schemaname = 'public' AND i.tablename <> '_sqlx_migrations'
  UNION ALL
    SELECT 'sequence ' || s.sequencename || ' ' || s.data_type::text || ' start ' || s.start_value
        || ' inc ' || s.increment_by || ' min ' || s.min_value || ' max ' || s.max_value
        || ' cache ' || s.cache_size || CASE WHEN s.cycle THEN ' cycle' ELSE '' END
        || coalesce(' owned by ' || (
            SELECT t.relname || '.' || a.attname
            FROM pg_depend dep
            JOIN pg_class seq ON seq.oid = dep.objid
            JOIN pg_class t ON t.oid = dep.refobjid
            JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = dep.refobjsubid
            WHERE seq.relname = s.sequencename AND seq.relkind = 'S' AND dep.deptype = 'a'
        ), '')
    FROM pg_sequences s
    WHERE s.schemaname = 'public' AND s.sequencename <> '_sqlx_migrations_id_seq'
  UNION ALL
    SELECT 'view ' || v.viewname || ' ' || v.definition
    FROM pg_views v WHERE v.schemaname = 'public'
  UNION ALL
    SELECT 'function ' || p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')'
    FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'public'
  UNION ALL
    SELECT 'trigger ' || t.tgname || ' ' || pg_get_triggerdef(t.oid, true)
    FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND NOT t.tgisinternal
  UNION ALL
    SELECT 'extension ' || e.extname FROM pg_extension e
  UNION ALL
    SELECT 'schema_migrations ' || version FROM public.schema_migrations
  UNION ALL
    SELECT 'ar_internal_metadata ' || key
    FROM public.ar_internal_metadata WHERE key = 'environment'
)
SELECT line FROM lines ORDER BY line COLLATE "C"
