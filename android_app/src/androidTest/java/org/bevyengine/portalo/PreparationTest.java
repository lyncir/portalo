package org.bevyengine.portalo;

import android.content.res.AssetFileDescriptor;
import android.os.ParcelFileDescriptor;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.concurrent.*;
import org.junit.Test;
import org.junit.runner.RunWith;
import static org.junit.Assert.*;

/** Does not need a selected SAF folder: pipes model a provider that opens
 * successfully but never returns data or EOF. */
@RunWith(AndroidJUnit4.class)
public class PreparationTest {
    @Test
    public void cancellingIdlePipeReleasesReaderAndAllowsNextTask() throws Exception {
        ExecutorService worker = Executors.newSingleThreadExecutor();
        try {
            for (int round = 0; round < 3; round++) {
                ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
                java.io.FileDescriptor inputFd = pipe[0].getFileDescriptor();
                File target = File.createTempFile("prepare-test-", ".part",
                        InstrumentationRegistry.getInstrumentation().getTargetContext().getCacheDir());
                MainActivity.Preparation job = new MainActivity.Preparation();
                CountDownLatch entered = new CountDownLatch(1);
                try (ParcelFileDescriptor writer = pipe[1]) {
                    Future<?> task = worker.submit(() -> {
                        try (AssetFileDescriptor asset = new AssetFileDescriptor(pipe[0], 0, AssetFileDescriptor.UNKNOWN_LENGTH);
                             FileOutputStream output = new FileOutputStream(target)) {
                            entered.countDown();
                            MainActivity.copyToCache(job, asset, output);
                            throw new AssertionError("Idle pipe completed without cancellation");
                        } catch (IOException expected) {
                            assertTrue(expected.getMessage().contains("取消"));
                        }
                        return null;
                    });
                    assertTrue(entered.await(2, TimeUnit.SECONDS));
                    // Leave the write end open and idle even after cancellation.
                    Thread.sleep(150);
                    synchronized (job) { job.done = true; }
                    task.get(2, TimeUnit.SECONDS);
                    assertFalse(inputFd.valid());
                } finally { pipe[0].close(); target.delete(); }
            }
        } finally { worker.shutdownNow(); }
    }

    @Test
    public void knownLengthPipeStopsAtDocumentBoundaryWithoutWaitingForEof() throws Exception {
        ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
        File target = File.createTempFile("prepare-boundary-", ".part",
                InstrumentationRegistry.getInstrumentation().getTargetContext().getCacheDir());
        ExecutorService worker = Executors.newSingleThreadExecutor();
        try (ParcelFileDescriptor.AutoCloseOutputStream writer = new ParcelFileDescriptor.AutoCloseOutputStream(pipe[1])) {
            writer.write(new byte[] {1, 2, 3, 4});
            Future<?> task = worker.submit(() -> {
                try (AssetFileDescriptor asset = new AssetFileDescriptor(pipe[0], 0, 3);
                     FileOutputStream output = new FileOutputStream(target)) {
                    MainActivity.copyToCache(new MainActivity.Preparation(), asset, output);
                }
                return null;
            });
            task.get(2, TimeUnit.SECONDS);
            assertArrayEquals(new byte[] {1, 2, 3}, java.nio.file.Files.readAllBytes(target.toPath()));
        } finally { pipe[0].close(); worker.shutdownNow(); target.delete(); }
    }

    @Test
    public void closedStorageRejectsBeforeParsingProviderUri() throws Exception {
        SafStorage storage = new SafStorage(InstrumentationRegistry.getInstrumentation().getTargetContext());
        storage.close();
        try {
            storage.validate("not-a-provider-uri");
            fail("Closed storage accepted an operation");
        } catch (IOException expected) {
            assertTrue(expected.getMessage().contains("关闭"));
        }
    }
}
