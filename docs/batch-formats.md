# Batch Input Formats

soundAr imports up to 1,000 rows from UTF-8 TXT, CSV, or JSONL files no larger than 8 MB. Empty
rows are skipped. Each speech text may contain up to 20,000 characters.

## TXT

Use one speech item per non-empty line:

```text
Welcome to the first chapter.
This is the second generated clip.
```

## CSV

CSV requires a `text` column. Quoted multiline values are supported. Optional columns are `name`,
`output_name`, `model_id`, `speaker`, `language`, `output_format`, `speed`, `seed`, `exaggeration`,
`cfg_weight`, `temperature`, `top_p`, `repetition_penalty`, and `priority`. Priority accepts `low`,
`normal`, `high`, or `urgent`; the batch-level default is `normal`.

```csv
name,text,output_name,priority,model_id,speed,seed
Intro,"Welcome, and thanks for listening.",intro,urgent,hexgrad/Kokoro-82M,1.0,42817
Outro,We will see you next time.,outro,normal,hexgrad/Kokoro-82M,0.95,42818
```

## JSONL

Each non-empty line is a JSON object. Put engine overrides in `settings`:

```json
{"name":"Intro","text":"Welcome to soundAr.","output_name":"intro","priority":"urgent","settings":{"model_id":"hexgrad/Kokoro-82M","speed":1.0,"seed":42817}}
{"name":"Outro","text":"Thanks for listening.","output_name":"outro","settings":{"speed":0.95}}
```

Unknown fields and unsupported settings are rejected with the source row number. File names are
normalized to path-safe ASCII stems such as `0001-intro`; each execution adds the batch identifier
and attempt number so retrying a failed row never overwrites an earlier artifact.
