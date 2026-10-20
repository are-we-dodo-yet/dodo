window.BENCHMARK_DATA = {
  "lastUpdate": 1792533830031,
  "repoUrl": "https://github.com/are-we-dodo-yet/dodo-staging-ci",
  "entries": {
    "Execution Time": [
      {
        "commit": {
          "author": {
            "email": "iam@lach.pw",
            "name": "Lach",
            "username": "CertainLach"
          },
          "committer": {
            "email": "iam@lach.pw",
            "name": "Lach",
            "username": "CertainLach"
          },
          "distinct": true,
          "id": "5a0df0bed104f64882b03f059b1e02fc6028d1b5",
          "message": "ci: also run on staging repo",
          "timestamp": "2026-10-20T23:30:08+02:00",
          "tree_id": "475066cba91b9dd87c26def032bea41645e5f698",
          "url": "https://github.com/are-we-dodo-yet/dodo-staging-ci/commit/5a0df0bed104f64882b03f059b1e02fc6028d1b5"
        },
        "date": 1792533819792,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold_hello",
            "value": 17.142,
            "range": "± 6.439",
            "unit": "ms",
            "extra": "min 12.845 ms, max 89.156 ms, user 7.296 ms, system 11.639 ms"
          },
          {
            "name": "cold_relative_import",
            "value": 16.857,
            "range": "± 2.723",
            "unit": "ms",
            "extra": "min 12.725 ms, max 39.707 ms, user 7.417 ms, system 11.985 ms"
          },
          {
            "name": "error_001",
            "value": 17.52,
            "range": "± 2.503",
            "unit": "ms",
            "extra": "min 13.174 ms, max 34.963 ms, user 7.954 ms, system 12.306 ms"
          },
          {
            "name": "hello",
            "value": 17.266,
            "range": "± 10.3",
            "unit": "ms",
            "extra": "min 13.131 ms, max 120.455 ms, user 7.394 ms, system 11.385 ms"
          },
          {
            "name": "no_check_hello",
            "value": 16.846,
            "range": "± 1.78",
            "unit": "ms",
            "extra": "min 13.093 ms, max 23.025 ms, user 7.783 ms, system 11.646 ms"
          },
          {
            "name": "relative_import",
            "value": 17.131,
            "range": "± 2.628",
            "unit": "ms",
            "extra": "min 13.221 ms, max 33.029 ms, user 7.572 ms, system 12.179 ms"
          },
          {
            "name": "response_string",
            "value": 23.616,
            "range": "± 3.104",
            "unit": "ms",
            "extra": "min 18.31 ms, max 43.257 ms, user 14.996 ms, system 15.144 ms"
          },
          {
            "name": "text_decoder",
            "value": 24.568,
            "range": "± 3.345",
            "unit": "ms",
            "extra": "min 19.801 ms, max 44.031 ms, user 14.962 ms, system 14.153 ms"
          },
          {
            "name": "text_encoder",
            "value": 59.69,
            "range": "± 4.385",
            "unit": "ms",
            "extra": "min 54.096 ms, max 70.116 ms, user 46.097 ms, system 23.084 ms"
          },
          {
            "name": "text_encoder_into",
            "value": 38.08,
            "range": "± 5.049",
            "unit": "ms",
            "extra": "min 31.586 ms, max 53.756 ms, user 30.995 ms, system 13.414 ms"
          },
          {
            "name": "workers_large_message",
            "value": 513.146,
            "range": "± 15.27",
            "unit": "ms",
            "extra": "min 485.982 ms, max 540.718 ms, user 395.892 ms, system 364.905 ms"
          },
          {
            "name": "workers_round_robin",
            "value": 95.092,
            "range": "± 2.588",
            "unit": "ms",
            "extra": "min 91.067 ms, max 102.4 ms, user 101.083 ms, system 49.372 ms"
          },
          {
            "name": "workers_startup",
            "value": 553.457,
            "range": "± 30.186",
            "unit": "ms",
            "extra": "min 530.789 ms, max 636.443 ms, user 389.428 ms, system 313.979 ms"
          }
        ]
      }
    ],
    "Thread Count": [
      {
        "commit": {
          "author": {
            "email": "iam@lach.pw",
            "name": "Lach",
            "username": "CertainLach"
          },
          "committer": {
            "email": "iam@lach.pw",
            "name": "Lach",
            "username": "CertainLach"
          },
          "distinct": true,
          "id": "5a0df0bed104f64882b03f059b1e02fc6028d1b5",
          "message": "ci: also run on staging repo",
          "timestamp": "2026-10-20T23:30:08+02:00",
          "tree_id": "475066cba91b9dd87c26def032bea41645e5f698",
          "url": "https://github.com/are-we-dodo-yet/dodo-staging-ci/commit/5a0df0bed104f64882b03f059b1e02fc6028d1b5"
        },
        "date": 1792533823236,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold_hello",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "cold_relative_import",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "error_001",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "hello",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "no_check_hello",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "relative_import",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "response_string",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "text_decoder",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "text_encoder",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "text_encoder_into",
            "value": 8,
            "unit": "threads"
          },
          {
            "name": "workers_large_message",
            "value": 58,
            "unit": "threads"
          },
          {
            "name": "workers_round_robin",
            "value": 12,
            "unit": "threads"
          },
          {
            "name": "workers_startup",
            "value": 59,
            "unit": "threads"
          }
        ]
      }
    ],
    "Syscall Count": [
      {
        "commit": {
          "author": {
            "email": "iam@lach.pw",
            "name": "Lach",
            "username": "CertainLach"
          },
          "committer": {
            "email": "iam@lach.pw",
            "name": "Lach",
            "username": "CertainLach"
          },
          "distinct": true,
          "id": "5a0df0bed104f64882b03f059b1e02fc6028d1b5",
          "message": "ci: also run on staging repo",
          "timestamp": "2026-10-20T23:30:08+02:00",
          "tree_id": "475066cba91b9dd87c26def032bea41645e5f698",
          "url": "https://github.com/are-we-dodo-yet/dodo-staging-ci/commit/5a0df0bed104f64882b03f059b1e02fc6028d1b5"
        },
        "date": 1792533825101,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold_hello",
            "value": 1867,
            "unit": "syscalls"
          },
          {
            "name": "cold_relative_import",
            "value": 1889,
            "unit": "syscalls"
          },
          {
            "name": "error_001",
            "value": 1934,
            "unit": "syscalls"
          },
          {
            "name": "hello",
            "value": 1874,
            "unit": "syscalls"
          },
          {
            "name": "no_check_hello",
            "value": 1881,
            "unit": "syscalls"
          },
          {
            "name": "relative_import",
            "value": 1889,
            "unit": "syscalls"
          },
          {
            "name": "response_string",
            "value": 2111,
            "unit": "syscalls"
          },
          {
            "name": "text_decoder",
            "value": 2020,
            "unit": "syscalls"
          },
          {
            "name": "text_encoder",
            "value": 2543,
            "unit": "syscalls"
          },
          {
            "name": "text_encoder_into",
            "value": 1960,
            "unit": "syscalls"
          },
          {
            "name": "workers_large_message",
            "value": 71426,
            "unit": "syscalls"
          },
          {
            "name": "workers_round_robin",
            "value": 8031,
            "unit": "syscalls"
          },
          {
            "name": "workers_startup",
            "value": 40019,
            "unit": "syscalls"
          }
        ]
      }
    ],
    "Max Memory Usage": [
      {
        "commit": {
          "author": {
            "email": "iam@lach.pw",
            "name": "Lach",
            "username": "CertainLach"
          },
          "committer": {
            "email": "iam@lach.pw",
            "name": "Lach",
            "username": "CertainLach"
          },
          "distinct": true,
          "id": "5a0df0bed104f64882b03f059b1e02fc6028d1b5",
          "message": "ci: also run on staging repo",
          "timestamp": "2026-10-20T23:30:08+02:00",
          "tree_id": "475066cba91b9dd87c26def032bea41645e5f698",
          "url": "https://github.com/are-we-dodo-yet/dodo-staging-ci/commit/5a0df0bed104f64882b03f059b1e02fc6028d1b5"
        },
        "date": 1792533828048,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold_hello",
            "value": 47.578,
            "unit": "MiB"
          },
          {
            "name": "cold_relative_import",
            "value": 48.059,
            "unit": "MiB"
          },
          {
            "name": "error_001",
            "value": 46.953,
            "unit": "MiB"
          },
          {
            "name": "hello",
            "value": 47.387,
            "unit": "MiB"
          },
          {
            "name": "no_check_hello",
            "value": 47.48,
            "unit": "MiB"
          },
          {
            "name": "relative_import",
            "value": 47.887,
            "unit": "MiB"
          },
          {
            "name": "response_string",
            "value": 55.711,
            "unit": "MiB"
          },
          {
            "name": "text_decoder",
            "value": 55.883,
            "unit": "MiB"
          },
          {
            "name": "text_encoder",
            "value": 66.547,
            "unit": "MiB"
          },
          {
            "name": "text_encoder_into",
            "value": 56.047,
            "unit": "MiB"
          },
          {
            "name": "workers_large_message",
            "value": 429.465,
            "unit": "MiB"
          },
          {
            "name": "workers_round_robin",
            "value": 92.434,
            "unit": "MiB"
          },
          {
            "name": "workers_startup",
            "value": 417.453,
            "unit": "MiB"
          }
        ]
      }
    ]
  }
}