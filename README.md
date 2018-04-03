# Game Network

This library is a networking abstraction layer for [RollPlayGE](https://github.com/jakalope/roll_play_ge), though in principal it's general enough to serve other game engines just as well. It follows a client-server model and uses both TCP (reliable) and UDP (low-latency) to fulfill separate quality-of-service contracts for different kinds of data.

```
+---------------------------------------+       +--------------------------------------+
|                                       |       |                                      |
|  server::*                            |       |  client::*                           |
|                                       |       |                                      |
|  +--------+   +-----------------------+       +-----------------------+   +--------+ |
|  |        <---> low_latency::Servicer <----+--> low_latency::Servicer <--->        | |
|  | Server |   +-----------------------+    |  +-----------------------+   | Client | |
|  | Thread |   +-----------------------+    |  +-----------------------+   | Thread | |
|  |        <---> reliable::Servicer    <-------> reliable::Servicer    <--->        | |
|  |        |   +-----------------------+    |  +-----------------------+   +--------+ |
|  |        |              .            |    |  |                                      |
|  |        |              .            |    |  +--------------------------------------+
|  |        |              .            |    |                   .
|  |        |   +-----------------------+    |                   .
|  |        <---> reliable::Servicer    <--+ |                   .
|  +--------+   +-----------------------+  | |  +--------------------------------------+
|                                       |  | |  |                                      |
+---------------------------------------+  | |  |  client::*                           |
                                           | |  |                                      |
                                           | |  +-----------------------+   +--------+ |
                                           | +--+ low_latency::Servicer <--->        | |
                                           |    +-----------------------+   | Client | |
                                           |    +-----------------------+   | Thread | |
                                           +----+ reliable::Servicer    <--->        | |
                                                +-----------------------+   +--------+ |
                                                |                                      |
                                                +--------------------------------------+
```
