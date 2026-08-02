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

-- A line answered by the practice's own telephone system.
--
-- Until now every call went through a carrier, which means the caller's voice left the
-- deployment. A telephone system on the practice's own network can hand the audio
-- straight here instead, over their network and nowhere else, which is the only version
-- of this feature that matches what the rest of the platform promises.
--
-- Checked here as well as in code because the answer path compares this column exactly:
-- an unknown value would be a line that resolves to nothing rather than a line that
-- refuses, and the difference matters when somebody is ringing it.
ALTER TABLE phone_numbers DROP CONSTRAINT phone_numbers_provider_check;
ALTER TABLE phone_numbers ADD CONSTRAINT phone_numbers_provider_check
    CHECK (provider IN ('twilio', 'audiosocket'));
