DELETE FROM channels WHERE channel_type IN ('signal', 'whatsapp');
DELETE FROM message_log WHERE channel_type IN ('signal', 'whatsapp');
