-- Copyright 2026 Private AI Ltd (SC881079)
-- SPDX-License-Identifier: Apache-2.0
-- Absence is legacy; every opt-in edit invalidates previously minted tickets.
CREATE TABLE phone_routing (
    phone_number_id UUID PRIMARY KEY REFERENCES phone_numbers(id) ON DELETE CASCADE,
    revision UUID NOT NULL,
    model_selection JSONB NOT NULL CHECK (jsonb_typeof(model_selection)='object')
);
