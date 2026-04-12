---
name: "BrowserOperator"
description: "Operate interactive websites inside local Chrome, extract results, and fall back cleanly when the site blocks automation."
tags:
  - browser
  - chrome
  - web
  - automation
owner_agents:
  - general
  - researcher
  - sysadmin
recommended_workflows:
  - AskGeminiAndReturnAnswer
---
# Browser Operator

Use this skill for interactive browser tasks such as opening a site, submitting a prompt,
waiting for a result, and extracting page text back to the user.

## Guidance

- Prefer `chrome_browser` over low-level desktop actions when the task happens inside a website.
- Check page state first if the site may still be loading.
- Extract page text after submission instead of guessing what the site returned.
- If the page structure blocks generic automation, say so plainly and propose a narrower site-specific workflow.
- When a browser task becomes repetitive and reliable, propose a reusable workflow review item instead of keeping it ad hoc.
