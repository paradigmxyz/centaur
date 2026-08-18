-- ParadeDB 0.23 cannot combine paradedb.score with an RLS qualifier that
-- evaluates array membership on the indexed relation. Keep the policy keyed
-- by the scalar note_id and evaluate access against the canonical source row.
create or replace function centaur_company_context_granola_note_visible(
    p_note_id text
)
returns boolean
language sql
stable
security definer
set search_path = pg_catalog, public
as $$
    select exists (
        select 1
        from public.granola_sync_notes notes
        where notes.note_id = p_note_id
          and public.centaur_current_slack_user_email() = any(
              coalesce(notes.access_emails, array[]::text[])
          )
    )
$$;

revoke all on function centaur_company_context_granola_note_visible(text)
    from public;
grant execute on function centaur_company_context_granola_note_visible(text)
    to centaur_company_context_reader;

drop policy if exists centaur_cc_reader_granola_documents_select
    on granola_context_documents;
create policy centaur_cc_reader_granola_documents_select
    on granola_context_documents
    for select
    to centaur_company_context_reader
    using (centaur_company_context_granola_note_visible(note_id));
