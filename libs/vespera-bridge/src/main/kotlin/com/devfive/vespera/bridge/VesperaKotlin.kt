@file:JvmName("VesperaKotlin")

package com.devfive.vespera.bridge

import java.util.concurrent.Executor
import kotlinx.coroutines.future.await

/*
 * Kotlin extensions for the programmatic client (VesperaClient /
 * VesperaRequest / VesperaResponse).
 *
 * They ship inside the vespera-bridge JAR, but kotlin-stdlib and
 * kotlinx-coroutines-core are compileOnly: Java users never load this class
 * and get no Kotlin dependency. A Kotlin app already has kotlin-stdlib; only
 * `sendAwait` additionally needs kotlinx-coroutines-core (>= 1.7) on the
 * classpath.
 */

/**
 * Dispatch asynchronously ([VesperaRequest.sendAsync]) and suspend until the
 * response arrives — without blocking a thread.
 *
 * Cancelling the calling coroutine (e.g. `withTimeout`, a cancelled scope, a
 * disconnected WebFlux/Spring coroutine handler) cancels the underlying
 * `CompletableFuture`, which cancels the native dispatch: the Rust handler is
 * dropped at its next poll and its in-flight slot is released
 * (cancel-on-disconnect). HTTP error statuses are returned as responses;
 * transport failures throw [VesperaClientException].
 *
 * An `InputStream` body blocks while it is read: use the
 * `sendAwait(executor)` overload for it.
 *
 * Requires `org.jetbrains.kotlinx:kotlinx-coroutines-core` (>= 1.7).
 */
public suspend fun VesperaRequest.sendAwait(): VesperaResponse = sendAsync().await()

/**
 * [sendAwait] with the response decoded on [executor]
 * ([VesperaRequest.sendAsync] with an executor); required for an
 * `InputStream` body, which runs the blocking streaming dispatch on it.
 * Cancellation propagates to the native dispatch like [sendAwait] (a
 * streamed upload already running on [executor] is not interrupted).
 */
public suspend fun VesperaRequest.sendAwait(executor: Executor): VesperaResponse =
    sendAsync(executor).await()

/**
 * Build a request with a configuration block, e.g.
 *
 * ```kotlin
 * client.request("GET", "/users/42") { header("accept", "application/json") }
 * ```
 */
public inline fun VesperaClient.request(
    method: String,
    path: String,
    block: VesperaRequest.() -> Unit,
): VesperaRequest = request(method, path).apply(block)

/** `GET` [path] configured by [block]: `client.get("/users/42") { query("expand", "roles") }`. */
public inline fun VesperaClient.get(path: String, block: VesperaRequest.() -> Unit): VesperaRequest =
    get(path).apply(block)

/** `HEAD` [path] configured by [block]. */
public inline fun VesperaClient.head(path: String, block: VesperaRequest.() -> Unit): VesperaRequest =
    head(path).apply(block)

/** `POST` [path] configured by [block]: `client.post("/users") { json("""{"name":"a"}""") }`. */
public inline fun VesperaClient.post(path: String, block: VesperaRequest.() -> Unit): VesperaRequest =
    post(path).apply(block)

/** `PUT` [path] configured by [block]. */
public inline fun VesperaClient.put(path: String, block: VesperaRequest.() -> Unit): VesperaRequest =
    put(path).apply(block)

/** `PATCH` [path] configured by [block]. */
public inline fun VesperaClient.patch(path: String, block: VesperaRequest.() -> Unit): VesperaRequest =
    patch(path).apply(block)

/** `DELETE` [path] configured by [block]. */
public inline fun VesperaClient.delete(path: String, block: VesperaRequest.() -> Unit): VesperaRequest =
    delete(path).apply(block)

/** `OPTIONS` [path] configured by [block]. */
public inline fun VesperaClient.options(path: String, block: VesperaRequest.() -> Unit): VesperaRequest =
    options(path).apply(block)

/** Append every pair as [VesperaRequest.query] (a `null` value appends the bare name). */
public fun VesperaRequest.query(vararg params: Pair<String, Any?>): VesperaRequest = apply {
    for ((name, value) in params) query(name, value)
}

/** Set every pair as [VesperaRequest.header], replacing previous values. */
public fun VesperaRequest.headers(vararg headers: Pair<String, String>): VesperaRequest = apply {
    for ((name, value) in headers) header(name, value)
}
