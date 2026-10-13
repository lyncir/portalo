package org.bevyengine.portalo;

import android.os.Bundle;
import android.content.Intent;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.net.wifi.WifiManager;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.InputStream;
import java.io.FileOutputStream;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import com.google.androidgamesdk.GameActivity;

public class MainActivity extends GameActivity {
    private WifiManager.MulticastLock multicastLock;
    private final ExecutorService fileWorker = Executors.newSingleThreadExecutor();
    private boolean picking;
    private final SafStorage receiveStorage = new SafStorage(this);
    private native void nativeFilePicked(String path);
    private native void nativeBackRequested();
    private native void nativeInsetsChanged(float top, float bottom);
    private native void nativeDirectoryPicked(String uri, String label, String error);

    public void validateReceiveTree(String uri) throws java.io.IOException { receiveStorage.validate(uri); }
    public long beginReceiveDocument(String uri, String name) throws java.io.IOException { return receiveStorage.begin(uri, name); }
    public int receiveDocumentFd(long id) throws java.io.IOException { return receiveStorage.duplicateFd(id); }
    public void finishReceiveDocument(long id) throws java.io.IOException { receiveStorage.finish(id); }
    public void abortReceiveDocument(long id) throws java.io.IOException { receiveStorage.abort(id); }

    public void pickReceiveDirectory() {
        runOnUiThread(() -> {
            if (picking) { nativeDirectoryPicked("", "", "已有文件选择操作，请稍后重试"); return; }
            picking = true;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION | Intent.FLAG_GRANT_PREFIX_URI_PERMISSION);
            try { startActivityForResult(intent, 1002); }
            catch (Exception error) { picking = false; nativeDirectoryPicked("", "", "无法打开系统文件夹选择器：" + error.getMessage()); }
        });
    }

    public void copyText(String text) {
        runOnUiThread(() -> {
            android.content.ClipboardManager manager = (android.content.ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (manager != null) manager.setPrimaryClip(android.content.ClipData.newPlainText("Portalo", text));
        });
    }

    public void backgroundApp() { runOnUiThread(() -> moveTaskToBack(true)); }

    @Override
    public void onBackPressed() { nativeBackRequested(); }

    // GameActivity's input connection consumes BACK before onBackPressed on
    // some devices. Intercept it before forwarding keys to the native surface.
    @Override
    public boolean dispatchKeyEvent(android.view.KeyEvent event) {
        if (event.getKeyCode() == android.view.KeyEvent.KEYCODE_BACK) {
            if (event.getAction() == android.view.KeyEvent.ACTION_UP && !event.isCanceled()) nativeBackRequested();
            return true;
        }
        return super.dispatchKeyEvent(event);
    }

    public synchronized String getDeviceName() {
        android.content.SharedPreferences prefs = getPreferences(MODE_PRIVATE);
        String instance = prefs.getString("portalo_instance", null);
        if (instance == null) {
            instance = java.util.UUID.randomUUID().toString().substring(0, 8);
            prefs.edit().putString("portalo_instance", instance).apply();
        }
        return android.os.Build.MODEL + "-" + instance;
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        int surfaceColor = android.graphics.Color.rgb(245, 245, 242);
        getWindow().setStatusBarColor(surfaceColor);
        getWindow().setNavigationBarColor(surfaceColor);
        getWindow().getDecorView().setSystemUiVisibility(
                android.view.View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR
                        | android.view.View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
        getWindow().getDecorView().setOnApplyWindowInsetsListener((view, insets) -> {
            float density = getResources().getDisplayMetrics().density;
            nativeInsetsChanged(insets.getSystemWindowInsetTop() / density,
                    insets.getSystemWindowInsetBottom() / density);
            return view.onApplyWindowInsets(insets);
        });
        WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        if (wifi != null) {
            multicastLock = wifi.createMulticastLock("portalo-discovery");
            multicastLock.setReferenceCounted(false);
        }
    }

    @Override
    protected void onStart() {
        super.onStart();
        if (multicastLock != null && !multicastLock.isHeld()) multicastLock.acquire();
    }

    @Override
    protected void onStop() {
        if (multicastLock != null && multicastLock.isHeld()) multicastLock.release();
        super.onStop();
    }

    public void pickFile() {
        runOnUiThread(() -> {
            if (picking) { nativeFilePicked(""); return; }
            picking = true;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            startActivityForResult(intent, 1001);
        });
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == 1002) {
            if (result != RESULT_OK || data == null || data.getData() == null) {
                picking = false; nativeDirectoryPicked("", "", ""); return;
            }
            Uri tree = data.getData();
            int flags = data.getFlags() & (Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            fileWorker.execute(() -> {
                try {
                    getContentResolver().takePersistableUriPermission(tree, flags);
                    nativeDirectoryPicked(tree.toString(), receiveStorage.label(tree.toString()), "");
                } catch (Exception error) {
                    nativeDirectoryPicked("", "", "无法取得文件夹的长期读写权限，请重新选择：" + error.getMessage());
                } finally { runOnUiThread(() -> picking = false); }
            });
            return;
        }
        if (request != 1001) return;
        if (result != RESULT_OK || data == null || data.getData() == null) { picking = false; nativeFilePicked(""); return; }
        Uri uri = data.getData();
        fileWorker.execute(() -> {
            File output = null;
            try {
                String name = "selected-file";
                try (Cursor cursor = getContentResolver().query(uri, null, null, null, null)) {
                    if (cursor != null && cursor.moveToFirst()) {
                        int index = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                        if (index >= 0 && !cursor.isNull(index)) name = cursor.getString(index);
                    }
                }
                name = name.replace('/', '_').replace('\\', '_');
                if (name.isEmpty() || name.equals(".") || name.equals("..")) name = "selected-file";
                File directory = new File(getCacheDir(), "outgoing");
                if (!directory.isDirectory() && !directory.mkdirs()) throw new java.io.IOException("Cannot create outgoing cache");
                output = new File(directory, name);
                try (InputStream input = getContentResolver().openInputStream(uri);
                     FileOutputStream writer = new FileOutputStream(output)) {
                    if (input == null) throw new java.io.IOException("Cannot open selected file");
                    byte[] buffer = new byte[256 * 1024];
                    int count;
                    while ((count = input.read(buffer)) != -1) writer.write(buffer, 0, count);
                }
                nativeFilePicked(output.getAbsolutePath());
            } catch (Exception error) {
                if (output != null) output.delete();
                android.util.Log.e("Portalo", "File selection failed", error);
                nativeFilePicked("");
                runOnUiThread(() -> android.widget.Toast.makeText(this, "无法读取所选文件，请检查访问权限和剩余空间", android.widget.Toast.LENGTH_LONG).show());
            } finally { runOnUiThread(() -> picking = false); }
        });
    }

    @Override
    protected void onDestroy() {
        if (multicastLock != null && multicastLock.isHeld()) multicastLock.release();
        fileWorker.execute(receiveStorage::abortAll);
        fileWorker.shutdown();
        super.onDestroy();
    }
    static {
        System.loadLibrary("portalo");
    }

}
