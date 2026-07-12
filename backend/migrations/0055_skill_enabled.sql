-- Per-skill enable/disable. An admin can switch
-- a default/library skill off without code: a disabled skill stays in the DB + the
-- admin list but never enters the model's slot [2] (`chat::load_skills`) and
-- `read_skill` refuses it. Default true → existing skills keep working.
ALTER TABLE skills ADD COLUMN enabled BOOLEAN NOT NULL DEFAULT true;
