-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Legacy admission must not scan all retained jobs on every ordinary chat turn.
CREATE INDEX routed_jobs_chat_admission ON routed_jobs ((context->>'chat_id'));
