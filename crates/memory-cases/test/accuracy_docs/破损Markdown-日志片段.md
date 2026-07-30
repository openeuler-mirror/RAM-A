# Gateway hotfix draft

运维从聊天里复制内容时少了结尾代码围栏，下面的日志片段仍然应该被解析和索引。

```log
cart service intermittently returns HTTP 502
upstream checkout-api reset connection after idle timeout
temporary mitigation: lower keepalive timeout to 30s, reload nginx worker, and drain stale upstream sockets
