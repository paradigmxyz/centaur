-- The deployment's custom system prompt for agent turns, edited on the
-- Console's System Settings page. NULL means none is set. Only nanocode
-- threads apply it so far, after the brain's built-in base prompt.
ALTER TABLE public.system_settings ADD COLUMN system_prompt text;
