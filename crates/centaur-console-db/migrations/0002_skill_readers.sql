-- Read-only user grants on Console-authored skills, alongside skill_editors.
CREATE TABLE public.skill_readers (
    id bigserial PRIMARY KEY,
    skill_id bigint NOT NULL REFERENCES public.skills(id) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
    created_at timestamp(6) without time zone NOT NULL,
    updated_at timestamp(6) without time zone NOT NULL
);

CREATE UNIQUE INDEX index_skill_readers_on_skill_id_and_user_id
    ON public.skill_readers USING btree (skill_id, user_id);

CREATE INDEX index_skill_readers_on_user_id ON public.skill_readers USING btree (user_id);
