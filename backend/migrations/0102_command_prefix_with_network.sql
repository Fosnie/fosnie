-- Copyright 2026 Private AI Ltd (SC881079)
--
-- Licensed under the Apache License, Version 2.0 (the "License");
-- you may not use this file except in compliance with the License.
-- You may obtain a copy of the License at
--
--     http://www.apache.org/licenses/LICENSE-2.0
--
-- Unless required by applicable law or agreed to in writing, software
-- distributed under the License is distributed on an "AS IS" BASIS,
-- WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
-- See the License for the specific language governing permissions and
-- limitations under the License.

-- Whether an agreed command prefix also lets the command reach the network.
--
-- The default is the narrower grant: an agreement made before this existed, like
-- one made without asking for the network, covers only commands that run without
-- it. A command that needs the network is covered only by an agreement made with
-- the network, so widening the reach is always a deliberate, separate act.
ALTER TABLE workspace_command_prefixes
    ADD COLUMN with_network BOOLEAN NOT NULL DEFAULT false;
