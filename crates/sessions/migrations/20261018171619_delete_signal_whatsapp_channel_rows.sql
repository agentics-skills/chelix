DELETE FROM channel_sessions WHERE channel_type IN ('signal', 'whatsapp');
DELETE FROM session_prompt_queue
WHERE instr(content, '"channel_type":"signal"') > 0
   OR instr(content, '"channel_type":"whatsapp"') > 0;
UPDATE sessions
SET channel_binding = NULL
WHERE channel_binding IS NOT NULL
  AND (
    instr(channel_binding, '"channel_type":"signal"') > 0
    OR instr(channel_binding, '"channel_type":"whatsapp"') > 0
  );
