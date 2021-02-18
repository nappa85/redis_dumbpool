# redis_dumbpool
Really dumb implementation of a Redis Connection Pool

## Why another pooling crate?
My tests with other polling crates ended up with a new TIME_WAIT socket for every Redis query, like if the socket is discared and recreated every time.
Trying to demonstrate the problem, I endend up with this implementation, that is kind-of usable.

## How does it works?
There is a main object (Pool), storing a Vec of Redis connections.
The Pool can be inited with a number of active connections.
Every time a connection is requested from the pool, it's popped from the Vec and tested before being returned.
If a connection isn't available and the maximum number of connections has been already reached, an async sleep is performed before retrying.
On return, the connection is wrapped in a struct that only purpose is to return the connection to the pool on drop.
