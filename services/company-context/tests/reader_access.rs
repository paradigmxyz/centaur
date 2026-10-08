use std::{
    env,
    error::Error,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::{Connection, Executor, PgConnection, postgres::PgConnectOptions};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[tokio::test]
async fn company_context_reader_sees_only_drive_documents_observed_for_its_subject()
-> Result<(), Box<dyn Error>> {
    let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
        eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
        return Ok(());
    };
    let mut admin = PgConnection::connect(&database_url).await?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let name = format!("company_context_reader_{}_{nanos}", std::process::id());
    admin
        .execute(format!(r#"create database "{name}""#).as_str())
        .await?;

    let options = PgConnectOptions::from_str(&database_url)?.database(&name);
    let result = async {
        let mut conn = PgConnection::connect_with(&options).await?;
        let result = assert_reader_visibility(&mut conn).await;
        conn.close().await?;
        result
    }
    .await;
    // Terminates pg_search background mergers still connected to it.
    admin
        .execute(format!(r#"drop database if exists "{name}" with (force)"#).as_str())
        .await?;
    result
}

async fn assert_reader_visibility(conn: &mut PgConnection) -> Result<(), Box<dyn Error>> {
    // Mirror the service's migration setup.
    conn.execute("create schema if not exists company_context_system; set search_path to company_context_system, public")
        .await?;
    MIGRATOR.run(&mut *conn).await?;

    conn.execute(
        r#"
        insert into company_context_data.google_drive_broker_observations
            (broker_credential_id, file_id, provider_subject, active)
        values
            (1, 'file-viewer', 'subject-viewer', true),
            (2, 'file-other', 'subject-other', true),
            (3, 'file-revoked', 'subject-viewer', false),
            (4, 'file-shared', 'subject-viewer', true),
            (5, 'file-shared', 'subject-other', true),
            (6, 'file-blank-subject', '', true);

        insert into company_context_data.google_drive_documents
            (document_id, file_id, chunk_id, document_type, mime_type, title, body, content_hash)
        select 'google-drive:' || file_id || ':0', file_id, '0', 'google_doc',
               'application/vnd.google-apps.document', 'Roadmap', 'Roadmap launch plan', 'hash'
        from unnest(array[
            'file-viewer', 'file-other', 'file-revoked', 'file-shared', 'file-blank-subject'
        ]) as files(file_id);

        insert into company_context_data.google_drive_document_embeddings
            (document_id, model, dimensions, content_hash, embedding)
        select document_id, 'text-embedding-3-small', 1536, 'hash',
               array_fill(0.1::real, array[1536])::vector
        from company_context_data.google_drive_documents;
        "#,
    )
    .await?;

    let cases: [(Option<&str>, &[&str]); 5] = [
        // Own files plus the shared file; never another user's or a revoked file.
        (
            Some("subject-viewer"),
            &["google-drive:file-shared:0", "google-drive:file-viewer:0"],
        ),
        (
            Some("subject-other"),
            &["google-drive:file-other:0", "google-drive:file-shared:0"],
        ),
        // A subject with no observations sees nothing, including shared files.
        (Some("subject-third"), &[]),
        // Unset or blank subjects never match, even observations with a blank subject.
        (None, &[]),
        (Some(""), &[]),
    ];
    for (subject, expected) in cases {
        let expected: Vec<String> = expected.iter().map(|id| (*id).to_owned()).collect();
        assert_eq!(
            visible_document_ids(conn, subject).await?,
            (expected.clone(), expected),
            "google_subject = {subject:?}"
        );
    }

    let mut tx = conn.begin().await?;
    tx.execute("set local role centaur_company_context_reader")
        .await?;
    let error = sqlx::query("select 1 from company_context_data.google_drive_broker_observations")
        .fetch_all(&mut *tx)
        .await
        .expect_err("reader must not read observations directly");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    tx.rollback().await?;
    Ok(())
}

/// Returns (BM25 search hits, embedding rows) visible to the reader role.
async fn visible_document_ids(
    conn: &mut PgConnection,
    google_subject: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), Box<dyn Error>> {
    let mut tx = conn.begin().await?;
    tx.execute("set local role centaur_company_context_reader")
        .await?;
    if let Some(subject) = google_subject {
        sqlx::query("select set_config('centaur.google_subject', $1, true)")
            .bind(subject)
            .execute(&mut *tx)
            .await?;
    }
    // Same shape as the tool's keyword search. ParadeDB 0.23 fails with an
    // internal error if paradedb.score is filtered in WHERE under RLS.
    let mut search_hits: Vec<String> = sqlx::query_as::<_, (String, f32)>(
        r#"
        select document_id, paradedb.score(document_id)
        from company_context_data.google_drive_documents
        where body ||| 'roadmap'
        order by paradedb.score(document_id) desc, document_id
        "#,
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|(document_id, score)| {
        assert!(score > 0.0, "{document_id} must have a BM25 score");
        document_id
    })
    .collect();
    search_hits.sort();
    let embeddings = sqlx::query_scalar(
        "select document_id from company_context_data.google_drive_document_embeddings order by document_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.rollback().await?;
    Ok((search_hits, embeddings))
}
