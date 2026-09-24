/**
 * Multi-Channel Webhook Notifier (Discord & Telegram).
 * Dispatches structured, actionable alerts and recovery notifications.
 */

/**
 * Dispatch notification to Discord webhook.
 * @param {string} webhookUrl
 * @param {object} payload
 * @returns {Promise<boolean>}
 */
export async function sendDiscordAlert(webhookUrl, { title, description, color, fields, timestamp }) {
  if (!webhookUrl) return false;

  try {
    const embed = {
      title,
      description,
      color: color || 0xED4245, // Default Red
      fields: fields || [],
      timestamp: timestamp || new Date().toISOString(),
      footer: {
        text: 'Tessera Synthetic Health Monitor'
      }
    };

    const response = await fetch(webhookUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        username: 'Tessera Testnet Sentinel',
        embeds: [embed]
      })
    });

    if (!response.ok) {
      console.error(`[AlertNotifier] Discord webhook error HTTP ${response.status}: ${await response.text()}`);
      return false;
    }

    return true;
  } catch (err) {
    console.error(`[AlertNotifier] Failed to dispatch Discord alert:`, err.message);
    return false;
  }
}

/**
 * Dispatch notification to Telegram bot webhook / chat.
 * @param {string} botToken
 * @param {string} chatId
 * @param {string} text
 * @returns {Promise<boolean>}
 */
export async function sendTelegramAlert(botToken, chatId, text) {
  if (!botToken || !chatId) return false;

  try {
    const url = `https://api.telegram.org/bot${botToken}/sendMessage`;
    const response = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        chat_id: chatId,
        text,
        parse_mode: 'HTML',
        disable_web_page_preview: true
      })
    });

    if (!response.ok) {
      console.error(`[AlertNotifier] Telegram API error HTTP ${response.status}: ${await response.text()}`);
      return false;
    }

    return true;
  } catch (err) {
    console.error(`[AlertNotifier] Failed to dispatch Telegram alert:`, err.message);
    return false;
  }
}

/**
 * Unified Alert Dispatcher.
 * @param {object} config
 * @param {object} alertEvent
 */
export async function dispatchAlert(config, alertEvent) {
  const { probeName, isRecovery, consecutiveFailures, error, previousError, latencyMs, details } = alertEvent;
  const now = new Date().toISOString();

  // 1. Prepare Discord Embed
  const discordTitle = isRecovery
    ? `🟢 RECOVERED: ${probeName}`
    : `🔴 CRITICAL ALERT: ${probeName}`;

  const discordColor = isRecovery ? 0x57F287 : 0xED4245;

  const discordDescription = isRecovery
    ? `Probe **${probeName}** has recovered and is now answering health checks successfully.`
    : `Probe **${probeName}** failed ${consecutiveFailures} consecutive times!`;

  const fields = [
    { name: 'Target Probe', value: `\`${probeName}\``, inline: true },
    { name: 'Failures', value: `${consecutiveFailures}`, inline: true },
    { name: 'Latency', value: `${latencyMs || 0} ms`, inline: true }
  ];

  if (error) {
    fields.push({ name: 'Current Error', value: `\`\`\`${error.slice(0, 1000)}\`\`\``, inline: false });
  }

  if (isRecovery && previousError) {
    fields.push({ name: 'Resolved Issue', value: `\`\`\`${previousError.slice(0, 1000)}\`\`\``, inline: false });
  }

  if (details && Object.keys(details).length > 0) {
    fields.push({
      name: 'Diagnostic Details',
      value: `\`\`\`json\n${JSON.stringify(details, null, 2).slice(0, 1000)}\`\`\``,
      inline: false
    });
  }

  // 2. Prepare Telegram Message
  const telegramHeader = isRecovery
    ? `<b>🟢 RECOVERED: ${probeName}</b>`
    : `<b>🔴 CRITICAL ALERT: ${probeName}</b>`;

  const telegramBody = [
    telegramHeader,
    `<b>Environment:</b> ${config.environment}`,
    `<b>Consecutive Failures:</b> ${consecutiveFailures}`,
    `<b>Latency:</b> ${latencyMs || 0}ms`,
    error ? `<b>Error:</b> <code>${escapeHtml(error.slice(0, 500))}</code>` : '',
    previousError ? `<b>Previous Error:</b> <code>${escapeHtml(previousError.slice(0, 500))}</code>` : '',
    `<b>Timestamp:</b> ${now}`
  ].filter(Boolean).join('\n');

  // Dispatch in parallel
  const promises = [];
  if (config.discordWebhookUrl) {
    promises.push(
      sendDiscordAlert(config.discordWebhookUrl, {
        title: discordTitle,
        description: discordDescription,
        color: discordColor,
        fields,
        timestamp: now
      })
    );
  }

  if (config.telegramBotToken && config.telegramChatId) {
    promises.push(
      sendTelegramAlert(config.telegramBotToken, config.telegramChatId, telegramBody)
    );
  }

  await Promise.allSettled(promises);
}

function escapeHtml(str) {
  return str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}
