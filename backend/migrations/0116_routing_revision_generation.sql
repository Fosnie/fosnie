-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Generations must not repeat after delete/recreate (the ABA problem).
CREATE SEQUENCE routing_revision_generation AS BIGINT;
SELECT setval('routing_revision_generation', GREATEST(
    1, COALESCE((SELECT max(revision) FROM model_profiles), 1),
    COALESCE((SELECT max(revision) FROM routing_policies), 1)
));
