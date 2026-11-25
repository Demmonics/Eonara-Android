package com.lyricmotion.android

import android.app.AlertDialog
import android.app.NativeActivity
import android.content.ComponentName
import android.content.Intent
import android.graphics.Bitmap
import android.media.MediaMetadata
import android.media.session.MediaController
import android.media.session.MediaSessionManager
import android.media.session.PlaybackState
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.OpenableColumns
import android.provider.Settings
import android.view.MotionEvent
import android.content.ContentResolver
import android.widget.Toast
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.nio.charset.StandardCharsets

class LyricMotionActivity : NativeActivity() {
    companion object {
        private const val ARTWORK_SIZE = 256
        private const val REQUEST_IMPORT_LYRICS = 201
        private const val MAX_LYRICS_BYTES = 256_000
        private const val IMPORT_HOLD_MS = 650L
        private const val ACTION_PREVIOUS = 1 shl 0
        private const val ACTION_PLAY = 1 shl 1
        private const val ACTION_PAUSE = 1 shl 2
        private const val ACTION_NEXT = 1 shl 3
        private const val ACTION_SEEK = 1 shl 4

        @JvmStatic private external fun nativeOnAccessState(granted: Boolean)
        @JvmStatic private external fun nativeOnCacheRoot(path: String)
        @JvmStatic private external fun nativeOnSessionSelected(sessionId: String, epoch: Long)
        @JvmStatic private external fun nativeOnSessionCleared(epoch: Long)
        @JvmStatic
        private external fun nativeOnManualLyrics(
            sessionId: String?,
            trackId: String,
            epoch: Long,
            format: String,
            text: String,
        )
        @JvmStatic
        private external fun nativeOnPlaybackSnapshot(
            sessionId: String,
            epoch: Long,
            metadataRevision: Long,
            revision: Long,
            trackId: String?,
            title: String?,
            artist: String?,
            album: String?,
            positionMs: Long,
            durationMs: Long,
            playbackRate: Float,
            playbackState: Int,
            observedAtMs: Long,
            actions: Int,
            artworkRevision: Long,
        )

        @JvmStatic
        private external fun nativeOnArtwork(
            sessionId: String,
            epoch: Long,
            revision: Long,
            width: Int,
            height: Int,
            argbPixels: IntArray,
        )
    }

    private val mainHandler = Handler(Looper.getMainLooper())
    private lateinit var sessionManager: MediaSessionManager
    private var sessionsListener: MediaSessionManager.OnActiveSessionsChangedListener? = null
    private var selectedController: MediaController? = null
    private var selectedCallback: MediaController.Callback? = null
    private var selectedSessionId: String? = null
    private var sessionEpoch = 0L
    private var metadataRevision = 0L
    private var playbackRevision = 0L
    private var artworkRevision = 0L
    private var interceptingControls = false
    private var sessionPickerTouch = false
    private var activeControllers: List<MediaController> = emptyList()
    private var manualSessionId: String? = null
    private var selectedTrackId: String? = null
    private var pendingImportTarget: ImportTarget? = null
    private var importPressTriggered = false
    private var importPressDownX = 0f
    private var importPressDownY = 0f
    private var importPressRunnable: Runnable? = null

    private data class ImportTarget(
        val sessionId: String?,
        val trackId: String,
        val epoch: Long,
    )

    private val notificationListener by lazy {
        ComponentName(this, LyricMotionNotificationListener::class.java)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        nativeOnCacheRoot(cacheDir.resolve("lyrics").absolutePath)
        sessionManager = getSystemService(MediaSessionManager::class.java)
        if (!hasNotificationAccess()) {
            showNotificationAccessExplanation()
        }
    }

    override fun onResume() {
        super.onResume()
        refreshMediaAccess()
    }

    override fun onPause() {
        detachSessionObservers()
        super.onPause()
    }

    override fun onDestroy() {
        cancelImportPress()
        detachSessionObservers()
        super.onDestroy()
    }

    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_DOWN) {
            sessionPickerTouch =
                event.x >= widthPixels() * 0.78f && event.y <= heightPixels() * 0.16f
            if (sessionPickerTouch) {
                showSessionPicker()
                return true
            }
            val bottom = event.y >= heightPixels() * 0.76f
            interceptingControls = bottom && (selectedController != null || !hasNotificationAccess())
            scheduleImportPress(event)
        }
        if (sessionPickerTouch) {
            if (event.actionMasked == MotionEvent.ACTION_UP ||
                event.actionMasked == MotionEvent.ACTION_CANCEL
            ) {
                sessionPickerTouch = false
            }
            return true
        }
        if (event.actionMasked == MotionEvent.ACTION_MOVE &&
            (kotlin.math.abs(event.x - importPressDownX) > 32f ||
                kotlin.math.abs(event.y - importPressDownY) > 32f)
        ) {
            cancelImportPress()
        }
        if (!interceptingControls) {
            if (event.actionMasked == MotionEvent.ACTION_UP) {
                cancelImportPress()
                if (importPressTriggered) {
                    importPressTriggered = false
                    return true
                }
            }
            if (event.actionMasked == MotionEvent.ACTION_CANCEL) {
                cancelImportPress()
                if (importPressTriggered) {
                    importPressTriggered = false
                    return true
                }
            }
            return super.dispatchTouchEvent(event)
        }

        if (event.actionMasked == MotionEvent.ACTION_UP) {
            cancelImportPress()
            if (importPressTriggered) {
                importPressTriggered = false
                interceptingControls = false
                return true
            }
            handleTransportTouch(event.x / widthPixels(), event.y / heightPixels())
            interceptingControls = false
        } else if (event.actionMasked == MotionEvent.ACTION_CANCEL) {
            cancelImportPress()
            interceptingControls = false
        }
        return true
    }

    private fun refreshMediaAccess() {
        val granted = hasNotificationAccess()
        nativeOnAccessState(granted)
        detachSessionObservers()
        if (!granted) {
            clearSelection()
            return
        }

        val listener = MediaSessionManager.OnActiveSessionsChangedListener { controllers ->
            chooseActiveController(controllers.orEmpty())
        }
        sessionsListener = listener
        try {
            sessionManager.addOnActiveSessionsChangedListener(
                listener,
                notificationListener,
                mainHandler,
            )
            chooseActiveController(sessionManager.getActiveSessions(notificationListener))
        } catch (_: SecurityException) {
            nativeOnAccessState(false)
            clearSelection()
        }
    }

    private fun hasNotificationAccess(): Boolean {
        val enabled = Settings.Secure.getString(
            contentResolver,
            "enabled_notification_listeners",
        ) ?: return false
        val flattened = notificationListener.flattenToString()
        return enabled.split(':').any { it.equals(flattened, ignoreCase = true) }
    }

    private fun showNotificationAccessExplanation() {
        AlertDialog.Builder(this)
            .setTitle("Connect a music player")
            .setMessage(
                "LyricMotion needs notification access to observe active music sessions " +
                    "and show track details and lyrics. It does not read other notifications.",
            )
            .setNegativeButton("Not now", null)
            .setPositiveButton("Open settings") { _, _ ->
                try {
                    startActivity(Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS))
                } catch (_: android.content.ActivityNotFoundException) {
                    Toast.makeText(this, "Notification access settings are unavailable.", Toast.LENGTH_LONG)
                        .show()
                }
            }
            .show()
    }

    private fun chooseActiveController(controllers: List<MediaController>) {
        activeControllers = controllers
        val manuallySelected = manualSessionId?.let { selectedId ->
            controllers.firstOrNull { sessionIdentity(it) == selectedId }
        }
        if (manualSessionId != null && manuallySelected == null) {
            manualSessionId = null
        }
        if (manuallySelected != null) {
            selectController(manuallySelected)
            return
        }
        val candidate = controllers
            .filter { hasRelevantMedia(it) }
            .firstOrNull { it.playbackState?.state == PlaybackState.STATE_PLAYING }
            ?: controllers.firstOrNull { hasRelevantMedia(it) }
        selectController(candidate)
    }

    private fun hasRelevantMedia(controller: MediaController): Boolean {
        val metadata = controller.metadata
        return !metadata?.getString(MediaMetadata.METADATA_KEY_TITLE).isNullOrBlank() ||
            controller.playbackState?.state == PlaybackState.STATE_PLAYING
    }

    private fun selectController(controller: MediaController?) {
        val nextId = controller?.let(::sessionIdentity)
        if (nextId == selectedSessionId) {
            if (controller != null) {
                selectedController?.let { publishSnapshot(it, sessionEpoch) }
            } else {
                nativeOnSessionCleared(sessionEpoch)
            }
            return
        }

        detachSelectedController()
        sessionEpoch += 1
        metadataRevision = 0
        playbackRevision = 0
        artworkRevision = 0
        selectedTrackId = null
        selectedSessionId = nextId
        selectedController = controller
        if (controller == null || nextId == null) {
            nativeOnSessionCleared(sessionEpoch)
            return
        }

        nativeOnSessionSelected(nextId, sessionEpoch)
        val epoch = sessionEpoch
        val callback = object : MediaController.Callback() {
            override fun onMetadataChanged(metadata: MediaMetadata?) {
                publishSnapshot(controller, epoch, metadataChanged = true)
            }

            override fun onPlaybackStateChanged(state: PlaybackState?) {
                publishSnapshot(controller, epoch)
            }

            override fun onSessionDestroyed() {
                if (epoch == sessionEpoch) {
                    try {
                        chooseActiveController(
                            sessionManager.getActiveSessions(notificationListener),
                        )
                    } catch (_: SecurityException) {
                        nativeOnAccessState(false)
                        clearSelection()
                    }
                }
            }
        }
        selectedCallback = callback
        try {
            controller.registerCallback(callback, mainHandler)
            publishSnapshot(controller, epoch, metadataChanged = true)
        } catch (_: SecurityException) {
            nativeOnAccessState(false)
            clearSelection()
        }
    }

    private fun publishSnapshot(
        controller: MediaController,
        epoch: Long,
        metadataChanged: Boolean = false,
    ) {
        if (epoch != sessionEpoch || selectedController !== controller) return

        val metadata = controller.metadata
        val playback = controller.playbackState
        playbackRevision += 1
        if (metadataChanged) {
            metadataRevision += 1
            artworkRevision += 1
        }

        val title = metadata?.getString(MediaMetadata.METADATA_KEY_TITLE)
        val artist = metadata?.getString(MediaMetadata.METADATA_KEY_ARTIST)
            ?: metadata?.getString(MediaMetadata.METADATA_KEY_ALBUM_ARTIST)
        val album = metadata?.getString(MediaMetadata.METADATA_KEY_ALBUM)
        val duration = metadata?.getLong(MediaMetadata.METADATA_KEY_DURATION) ?: -1L
        val trackId = metadata?.getString(MediaMetadata.METADATA_KEY_MEDIA_ID)
            ?: listOf(controller.packageName, title.orEmpty(), artist.orEmpty(), duration)
                .joinToString("|")
        selectedTrackId = trackId
        pendingImportTarget?.let { target ->
            if (target.sessionId == selectedSessionId && target.trackId == trackId) {
                pendingImportTarget = target.copy(epoch = epoch)
            }
        }
        val actions = playback?.actions ?: 0L
        val currentlyPlaying = playback?.state == PlaybackState.STATE_PLAYING ||
            playback?.state == PlaybackState.STATE_FAST_FORWARDING ||
            playback?.state == PlaybackState.STATE_REWINDING
        val playPauseSupported = (actions and PlaybackState.ACTION_PLAY_PAUSE) != 0L
        val actionFlags =
            (if ((actions and PlaybackState.ACTION_SKIP_TO_PREVIOUS) != 0L) ACTION_PREVIOUS else 0) or
                (if ((actions and PlaybackState.ACTION_PLAY) != 0L ||
                    (playPauseSupported && !currentlyPlaying)
                ) ACTION_PLAY else 0) or
                (if ((actions and PlaybackState.ACTION_PAUSE) != 0L ||
                    (playPauseSupported && currentlyPlaying)
                ) ACTION_PAUSE else 0) or
                (if ((actions and PlaybackState.ACTION_SKIP_TO_NEXT) != 0L) ACTION_NEXT else 0) or
                (if ((actions and PlaybackState.ACTION_SEEK_TO) != 0L) ACTION_SEEK else 0)
        val state = when (playback?.state) {
            PlaybackState.STATE_PLAYING, PlaybackState.STATE_FAST_FORWARDING,
            PlaybackState.STATE_REWINDING -> 1
            PlaybackState.STATE_BUFFERING, PlaybackState.STATE_CONNECTING -> 2
            PlaybackState.STATE_STOPPED, PlaybackState.STATE_NONE -> 3
            else -> 0
        }
        nativeOnPlaybackSnapshot(
            selectedSessionId ?: return,
            epoch,
            metadataRevision,
            playbackRevision,
            trackId,
            title,
            artist,
            album,
            playback?.position ?: -1L,
            duration,
            playback?.playbackSpeed ?: 1f,
            state,
            SystemClock.elapsedRealtime(),
            actionFlags,
            artworkRevision,
        )
        if (metadataChanged) {
            publishArtwork(controller, metadata, epoch, artworkRevision)
        }
    }

    private fun publishArtwork(
        controller: MediaController,
        metadata: MediaMetadata?,
        epoch: Long,
        revision: Long,
    ) {
        if (epoch != sessionEpoch || selectedController !== controller) return
        val source = metadata?.getBitmap(MediaMetadata.METADATA_KEY_ART)
            ?: metadata?.getBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART)
        if (source == null || source.width == 0 || source.height == 0) {
            nativeOnArtwork(selectedSessionId ?: return, epoch, revision, 0, 0, intArrayOf())
            return
        }

        val edge = minOf(source.width, source.height)
        val crop = Bitmap.createBitmap(
            source,
            (source.width - edge) / 2,
            (source.height - edge) / 2,
            edge,
            edge,
        )
        val scaled = Bitmap.createScaledBitmap(crop, ARTWORK_SIZE, ARTWORK_SIZE, true)
        val pixels = IntArray(ARTWORK_SIZE * ARTWORK_SIZE)
        scaled.getPixels(pixels, 0, ARTWORK_SIZE, 0, 0, ARTWORK_SIZE, ARTWORK_SIZE)
        nativeOnArtwork(
            selectedSessionId ?: return,
            epoch,
            revision,
            ARTWORK_SIZE,
            ARTWORK_SIZE,
            pixels,
        )
        if (scaled !== crop && scaled !== source) scaled.recycle()
        if (crop !== source) crop.recycle()
    }

    private fun handleTransportTouch(xFraction: Float, yFraction: Float) {
        val controller = selectedController
        if (controller == null) {
            showNotificationAccessExplanation()
            return
        }
        val playback = controller.playbackState ?: return
        val actions = playback.actions
        val duration = controller.metadata?.getLong(MediaMetadata.METADATA_KEY_DURATION) ?: -1L
        val currentlyPlaying = playback.state == PlaybackState.STATE_PLAYING ||
            playback.state == PlaybackState.STATE_FAST_FORWARDING ||
            playback.state == PlaybackState.STATE_REWINDING
        val playPauseSupported = (actions and PlaybackState.ACTION_PLAY_PAUSE) != 0L

        if (yFraction < 0.81f && duration > 0 &&
            (actions and PlaybackState.ACTION_SEEK_TO) != 0L
        ) {
            controller.transportControls.seekTo((xFraction.coerceIn(0f, 1f) * duration).toLong())
            return
        }

        when {
            xFraction < 0.28f -> {
                if ((actions and PlaybackState.ACTION_SKIP_TO_PREVIOUS) != 0L) {
                    controller.transportControls.skipToPrevious()
                }
            }
            xFraction > 0.72f -> {
                if ((actions and PlaybackState.ACTION_SKIP_TO_NEXT) != 0L) {
                    controller.transportControls.skipToNext()
                }
            }
            currentlyPlaying &&
                ((actions and PlaybackState.ACTION_PAUSE) != 0L || playPauseSupported) ->
                controller.transportControls.pause()
            !currentlyPlaying &&
                ((actions and PlaybackState.ACTION_PLAY) != 0L || playPauseSupported) ->
                controller.transportControls.play()
        }
    }

    private fun clearSelection() {
        detachSelectedController()
        sessionEpoch += 1
        metadataRevision = 0
        playbackRevision = 0
        artworkRevision = 0
        selectedTrackId = null
        activeControllers = emptyList()
        manualSessionId = null
        nativeOnSessionCleared(sessionEpoch)
    }

    private fun detachSessionObservers() {
        sessionsListener?.let(sessionManager::removeOnActiveSessionsChangedListener)
        sessionsListener = null
        detachSelectedController()
    }

    private fun detachSelectedController() {
        selectedCallback?.let { callback ->
            selectedController?.unregisterCallback(callback)
        }
        selectedCallback = null
        selectedController = null
        selectedSessionId = null
    }

    private fun scheduleImportPress(event: MotionEvent) {
        cancelImportPress()
        importPressTriggered = false
        importPressDownX = event.x
        importPressDownY = event.y
        val runnable = Runnable {
            importPressTriggered = true
            openLyricsPicker()
        }
        importPressRunnable = runnable
        mainHandler.postDelayed(runnable, IMPORT_HOLD_MS)
    }

    private fun cancelImportPress() {
        importPressRunnable?.let(mainHandler::removeCallbacks)
        importPressRunnable = null
    }

    private fun openLyricsPicker() {
        val target = if (selectedController != null) {
            val trackId = selectedTrackId
            val sessionId = selectedSessionId
            if (trackId == null || sessionId == null) {
                Toast.makeText(this, "Track information is not ready.", Toast.LENGTH_SHORT).show()
                return
            }
            ImportTarget(sessionId, trackId, sessionEpoch)
        } else {
            ImportTarget(null, "lyricmotion-demo", 0)
        }
        pendingImportTarget = target
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
            putExtra(
                Intent.EXTRA_MIME_TYPES,
                arrayOf(
                    "text/plain",
                    "text/xml",
                    "application/xml",
                    "application/ttml+xml",
                    "application/yaml",
                    "text/yaml",
                ),
            )
        }
        try {
            startActivityForResult(intent, REQUEST_IMPORT_LYRICS)
        } catch (_: android.content.ActivityNotFoundException) {
            pendingImportTarget = null
            Toast.makeText(this, "No file picker is available.", Toast.LENGTH_LONG).show()
        }
    }

    @Deprecated("Use the activity result API when this NativeActivity supports ActivityResultRegistry.")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQUEST_IMPORT_LYRICS) return
        val target = pendingImportTarget
        pendingImportTarget = null
        val uri = data?.data
        if (resultCode != RESULT_OK || uri == null || target == null) return
        try {
            val bytes = contentResolver.openInputStream(uri)?.use(::readBoundedLyrics)
                ?: throw IOException("The selected file could not be opened.")
            val text = String(bytes, StandardCharsets.UTF_8)
            val format = displayName(contentResolver, uri)
                ?.substringAfterLast('.', "auto")
                ?.lowercase()
                ?: "auto"
            nativeOnManualLyrics(target.sessionId, target.trackId, target.epoch, format, text)
        } catch (error: IOException) {
            Toast.makeText(
                this,
                error.message ?: "The selected lyrics file could not be read.",
                Toast.LENGTH_LONG,
            ).show()
        } catch (_: SecurityException) {
            Toast.makeText(this, "Permission to read the selected file was denied.", Toast.LENGTH_LONG)
                .show()
        }
    }

    private fun readBoundedLyrics(input: java.io.InputStream): ByteArray {
        val output = ByteArrayOutputStream()
        val buffer = ByteArray(8_192)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            if (output.size() + count > MAX_LYRICS_BYTES) {
                throw IOException("Lyrics files must be 256 KB or smaller.")
            }
            output.write(buffer, 0, count)
        }
        return output.toByteArray()
    }

    private fun displayName(resolver: ContentResolver, uri: android.net.Uri): String? {
        resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                val column = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (column >= 0) return cursor.getString(column)
            }
        }
        return null
    }

    private fun showSessionPicker() {
        if (!hasNotificationAccess()) {
            showNotificationAccessExplanation()
            return
        }
        val controllers = activeControllers.filter(::hasRelevantMedia)
        val labels = mutableListOf("Automatic")
        labels += controllers.map { controller ->
            val title = controller.metadata
                ?.getString(MediaMetadata.METADATA_KEY_TITLE)
                .orEmpty()
                .ifBlank { controller.packageName }
            val artist = controller.metadata
                ?.getString(MediaMetadata.METADATA_KEY_ARTIST)
                .orEmpty()
            if (artist.isBlank()) title else "$title — $artist"
        }
        val selectedIndex = manualSessionId?.let { id ->
            controllers.indexOfFirst { sessionIdentity(it) == id }.takeIf { it >= 0 }?.plus(1)
        } ?: 0
        AlertDialog.Builder(this)
            .setTitle("Choose music player")
            .setSingleChoiceItems(labels.toTypedArray(), selectedIndex) { dialog, which ->
                if (which == 0) {
                    manualSessionId = null
                    chooseActiveController(activeControllers)
                } else {
                    val controller = controllers.getOrNull(which - 1)
                    if (controller != null) {
                        manualSessionId = sessionIdentity(controller)
                        selectController(controller)
                    }
                }
                dialog.dismiss()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun sessionIdentity(controller: MediaController) =
        "${controller.packageName}/${controller.sessionToken.hashCode()}"

    private fun widthPixels() = window.decorView.width.coerceAtLeast(1).toFloat()
    private fun heightPixels() = window.decorView.height.coerceAtLeast(1).toFloat()
}
