# Game Network

This library is a networking abstraction layer for [RollPlayGE](https://github.com/jakalope/roll_play_ge), though in principal it's general enough to serve other game engines just as well. It follows a client-server model and uses both TCP (reliable) servicer threads and UDP (low-latency) servicer threads to fulfill separate quality-of-service contracts for different kinds of data.

The server has a single UDP servicer and `N` TCP servicers, while each of `N` clients have one UDP and one TCP servicer. Each servicer thread communicates bi-directionally via in-process queues with the main thread.

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
