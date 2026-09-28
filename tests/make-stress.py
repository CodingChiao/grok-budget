import json, re, io

data = {
  "quota": {
    "used_percent": 74, "remaining_percent": 26,
    "period_start": "2026-09-22T16:00:00+00:00", "period_end": "2026-09-29T16:00:00+00:00",
    "products": [{"product":"GrokBuild","used_percent":71},{"product":"GrokChat","used_percent":3}]
  },
  "live_local": {
    "cost_usd": 21.40792, "totalTokens": 81234567.0, "inputTokens": 78234567.0,
    "outputTokens": 3000000.0, "cachedReadTokens": 66000000.0, "cacheCreationTokens": 120000.0,
    "modelCalls": 912.0, "turns": 63.0, "sessions": 14.0, "ledger_files": 142.0,
    "duplicates": 2.0, "unreadable_files": 0.0, "partial_cost_turns": 3.0, "undated_turns": 0.0,
    "daily": {
      "2026-09-21": 0.8421, "2026-09-22": 2.1134, "2026-09-23": 0.3102, "2026-09-24": 4.9982,
      "2026-09-25": 1.2205, "2026-09-26": 6.5540, "2026-09-27": 2.0311, "2026-09-28": 3.3184, "2026-09-29": 0.0200
    },
    "models": {
      "grok-4.7-build": {"calls": 701.0, "cost_usd": 18.20211, "input_tokens": 66000000.0, "output_tokens": 2400000.0, "cached_tokens": 55000000.0, "partial": False},
      "grok-code-fast-1": {"calls": 188.0, "cost_usd": 3.10181, "input_tokens": 11000000.0, "output_tokens": 540000.0, "cached_tokens": 9800000.0, "partial": True},
      "grok-3-mini": {"calls": 23.0, "cost_usd": 0.10400, "input_tokens": 1234567.0, "output_tokens": 60000.0, "cached_tokens": 1000000.0, "partial": False}
    }
  },
  "estimate": {"available": True, "usd": 28.36, "low_usd": 17.02, "high_usd": 85.09, "confidence": "参考估算",
    "reason": "同周期成本增量 ÷ GrokBuild 产品占比增量 × 100；假设本机覆盖全部 Build 消耗且计费权重稳定。"},
  "projection": {"available": True, "summary": "近 3.0 小时内，GrokBuild 产品占比约 +1.90 个百分点/小时；照此约 13 小时后用满（外推，不是官方额度）",
    "reason": "近 3.0 小时内，GrokBuild 产品占比约 +1.90 个百分点/小时；照此约 13 小时后用满（外推，不是官方额度）"},
  "sample_count": 41, "sampled_at_iso": "2026-09-29T03:10:01+00:00", "cache_age_seconds": 26,
  "stale": False, "source": "https://cli-chat-proxy.grok.com/v1/billing?format=credits"
}

tpl = io.open("output/grok-budget.html", encoding="utf-8").read()
inj = json.dumps(data, ensure_ascii=False).replace("<", "\u003c").replace("&", "\u0026")
out = re.sub(r'(<script id="report-data" type="application/json">).*?(</script>)',
             lambda m: m.group(1) + inj + m.group(2), tpl, count=1, flags=re.S)
io.open("target/tmp/stress.html", "w", encoding="utf-8").write(out)
print("stress page written")
