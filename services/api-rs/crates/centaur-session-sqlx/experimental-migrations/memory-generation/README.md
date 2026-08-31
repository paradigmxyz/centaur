# Experimental Memory Generation Schema

`0001_create_memories.sql` is intentionally not part of the embedded SQLx
migration sequence. Apply it manually only to disposable or explicitly
participating environments.

The file remains editable while the experiment is active. Drop and recreate
the experimental tables when its shape changes. Once the schema is stable,
replace it with new numbered SQLx migrations and remove this directory.
