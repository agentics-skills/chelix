-- Remove the obsolete paired-node authentication and request state.

DROP INDEX IF EXISTS idx_device_tokens_prefix;
DROP INDEX IF EXISTS idx_device_tokens_device;
DROP INDEX IF EXISTS idx_pair_requests_public_key;
DROP INDEX IF EXISTS idx_pair_requests_status;
DROP INDEX IF EXISTS idx_paired_devices_public_key;

DROP TABLE IF EXISTS device_tokens;
DROP TABLE IF EXISTS pair_requests;
DROP TABLE IF EXISTS paired_devices;
