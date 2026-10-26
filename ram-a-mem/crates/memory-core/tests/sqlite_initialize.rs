// Copyright (c) 2026 Huawei Technologies Co.,Ltd. All rights reserved.
//
// RAM-A is licensed under Mulan PSL v2.
// You can use this software according to the terms and conditions of the Mulan
// PSL v2.
// You may obtain a copy of Mulan PSL v2 at:
//         http://license.coscl.org.cn/MulanPSL2
// THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
// KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
// NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
// See the Mulan PSL v2 for more details.

use memory_core::SqliteMemoryStore;

#[tokio::test]
async fn explicit_initialize_creates_the_memory_schema_without_a_memory_operation() {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("nested").join("memory.sqlite");
    let store = SqliteMemoryStore::new(&database_path);
    assert!(!database_path.exists());

    store.initialize().await.unwrap();
    store.initialize().await.unwrap();

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    for table in ["memories", "memory_fts"] {
        let exists = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get::<_, bool>(0),
            )
            .unwrap();
        assert!(exists, "missing {table}");
    }
}
