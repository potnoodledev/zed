-- Make github_user_id nullable for AFFiNE-only users
ALTER TABLE public.users ALTER COLUMN github_user_id DROP NOT NULL;
ALTER TABLE public.users ALTER COLUMN github_user_id SET DEFAULT NULL;

-- PostgreSQL already treats NULLs as distinct in unique indexes,
-- so the existing unique index on github_user_id allows multiple NULLs.

-- Add AFFiNE identity and avatar columns
ALTER TABLE public.users ADD COLUMN IF NOT EXISTS affine_user_id VARCHAR(255);
ALTER TABLE public.users ADD COLUMN IF NOT EXISTS avatar_url VARCHAR(512);

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_affine_user_id
    ON public.users (affine_user_id) WHERE affine_user_id IS NOT NULL;
