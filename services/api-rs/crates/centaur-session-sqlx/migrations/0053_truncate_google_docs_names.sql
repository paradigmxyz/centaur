drop index if exists idx_google_docs_sync_files_name;

update google_docs_sync_files
set name = left(name, 255)
where length(name) > 255;

update google_docs_sync_file_observations
set observed_name = left(observed_name, 255)
where length(observed_name) > 255;

alter table google_docs_sync_files
    add constraint google_docs_sync_files_name_length
    check (length(name) <= 255);

alter table google_docs_sync_file_observations
    add constraint google_docs_sync_file_observations_name_length
    check (length(observed_name) <= 255);

create index idx_google_docs_sync_files_name
    on google_docs_sync_files (name);
