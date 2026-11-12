package com.devfive.vespera.bridge

import com.fasterxml.jackson.databind.ObjectMapper
import java.io.ByteArrayInputStream
import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.util.concurrent.CompletableFuture
import java.util.concurrent.Executors
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.TimeoutCancellationException
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

/** Kotlin extensions (`VesperaKotlin.kt`) over the fake [VesperaClient.Transport] seam. */
class VesperaKotlinTest {

    /** Records wire requests; async dispatches answer [asyncResponse] (or stay pending). */
    private class FakeTransport : VesperaClient.Transport {
        val requests = mutableListOf<ByteArray>()
        val pending = mutableListOf<CompletableFuture<ByteArray>>()
        var asyncResponse: CompletableFuture<ByteArray>? = null
        var failure: RuntimeException? = null
        var uploaded: ByteArray? = null

        override fun dispatch(wireRequest: ByteArray): ByteArray {
            requests += wireRequest
            return OK
        }

        override fun dispatchAsync(wireRequest: ByteArray): CompletableFuture<ByteArray> {
            requests += wireRequest
            failure?.let { throw it }
            val future = asyncResponse ?: CompletableFuture<ByteArray>()
            pending += future
            return future
        }

        override fun dispatchStreaming(
            wireRequestHeader: ByteArray,
            input: InputStream,
            out: OutputStream,
        ): ByteArray {
            requests += wireRequestHeader
            uploaded = input.readAllBytes()
            out.write("streamed".toByteArray())
            return OK
        }
    }

    private val transport = FakeTransport()
    private val client = VesperaClient(transport, null)

    @Test
    fun `dsl builders configure the request with a receiver block`() {
        val get = client.get("/users/42") {
            header("Accept", "application/json")
            query("expand", "roles teams")
            query("page" to 2, "flag" to null)
            headers("X-A" to "1", "X-B" to "2")
        }
        assertEquals("GET", get.method())
        assertEquals("/users/42", get.path())
        assertEquals("expand=roles%20teams&page=2&flag", get.queryString())
        assertEquals("application/json", get.headerValue("accept"))
        assertEquals("1", get.headerValue("x-a"))
        assertEquals("2", get.headerValue("x-b"))

        val methods = listOf(
            client.head("/h") {},
            client.post("/p") { json("""{"name":"a"}""") },
            client.put("/u") {},
            client.patch("/pa") {},
            client.delete("/d") {},
            client.options("/o") {},
            client.request("PURGE", "/c") { app("admin") },
        ).map { it.method() }
        assertEquals(listOf("HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "PURGE"), methods)
        assertEquals("admin", client.request("PURGE", "/c") { app("admin") }.appName())

        val response = client.post("/users") { json("""{"name":"a"}""") }.send()
        val wire = transport.requests.single()
        val headerLen = ByteBuffer.wrap(wire).int
        val header = ObjectMapper().readTree(String(wire, 4, headerLen, Charsets.UTF_8))
        assertEquals("POST", header["method"].asText())
        assertEquals("application/json", header["headers"]["content-type"].asText())
        assertArrayEquals("""{"name":"a"}""".toByteArray(), wire.copyOfRange(4 + headerLen, wire.size))
        assertEquals(200, response.status())
    }

    @Test
    fun `nullable API surfaces as Kotlin nullable types`() {
        val response = client.get("/x").send()
        // JSpecify @Nullable -> String? (an absent header is null, not a platform-type NPE).
        val missing: String? = response.header("x-missing")
        assertNull(missing)
        assertEquals("text/plain", response.contentType())
        assertNull(client.appName())
        assertNull(client.get("/x").headerValue("nope"))
        val body: String = response.bodyString()
        assertEquals("ok", body)
    }

    @Test
    fun `sendAwait suspends until the async dispatch completes`() = runBlocking {
        transport.asyncResponse = CompletableFuture.completedFuture(OK)
        val response = client.get("/users/42") { header("accept", "text/plain") }.sendAwait()
        assertEquals(200, response.status())
        assertEquals("ok", response.bodyString())
        assertEquals(1, transport.requests.size)
    }

    @Test
    fun `sendAwait resumes when the response arrives later`() = runBlocking {
        val deferred = async(start = CoroutineStart.UNDISPATCHED) { client.get("/slow").sendAwait() }
        assertFalse(deferred.isCompleted)
        transport.pending.single().complete(OK)
        assertEquals("ok", deferred.await().bodyString())
    }

    @Test
    fun `cancelling the coroutine cancels the native dispatch future`() = runBlocking {
        val deferred = async(start = CoroutineStart.UNDISPATCHED) { client.get("/slow").sendAwait() }
        val native = transport.pending.single()
        assertFalse(native.isDone)
        deferred.cancel()
        assertTrue(runCatching { deferred.await() }.exceptionOrNull() is CancellationException)
        assertTrue(native.isCancelled, "coroutine cancellation reaches the dispatch future (cancel0)")
    }

    @Test
    fun `withTimeout cancels the native dispatch future`() = runBlocking {
        val timedOut = runCatching { withTimeout(50) { client.get("/never").sendAwait() } }
        assertTrue(timedOut.exceptionOrNull() is TimeoutCancellationException)
        assertTrue(transport.pending.single().isCancelled)
    }

    @Test
    fun `transport failures surface as VesperaClientException`() {
        val failure = VesperaClientException("vespera native library is not initialized")
        transport.failure = failure
        val thrown = assertThrows<VesperaClientException> {
            runBlocking { client.get("/x").sendAwait() }
        }
        assertSame(failure, thrown)
    }

    @Test
    fun `sendAwait with an executor streams an InputStream body`() = runBlocking {
        val executor = Executors.newSingleThreadExecutor()
        try {
            val response = client.post("/upload") {
                body(ByteArrayInputStream("payload".toByteArray()), "application/octet-stream")
            }.sendAwait(executor)
            assertEquals("streamed", response.bodyString())
            assertEquals("payload", transport.uploaded?.decodeToString())

            transport.asyncResponse = CompletableFuture.completedFuture(OK)
            assertEquals("ok", client.get("/x").sendAwait(executor).bodyString())
        } finally {
            executor.shutdownNow()
        }
    }

    @Test
    fun `sendAwait rejects an InputStream body without an executor`() {
        val request = client.post("/upload") { body(ByteArrayInputStream(ByteArray(1)), null) }
        assertThrows<IllegalStateException> { runBlocking { request.sendAwait() } }
        assertTrue(transport.requests.isEmpty())
    }

    private companion object {
        val OK: ByteArray = VesperaClientTest.responseWire(200, """{"content-type":"text/plain"}""", null, "ok")
    }
}
