package io.gratia.app.security

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Backspace
import androidx.compose.material.icons.filled.Fingerprint
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.gratia.app.GratiaLogo
import io.gratia.app.ui.GratiaHaptics
import io.gratia.app.ui.theme.AmberGold
import io.gratia.app.ui.theme.DeepNavy
import io.gratia.app.ui.theme.WarmWhite
import androidx.compose.ui.platform.LocalContext

// ============================================================================
// Main Lock Screen
// ============================================================================

/**
 * Full-screen lock overlay. Shows the appropriate auth UI based on the
 * configured lock method.
 *
 * @param lockMethod The current lock method to display
 * @param onPinVerified Called when PIN is successfully verified
 * @param onPatternVerified Called when pattern is successfully verified
 * @param onBiometricRequest Called to trigger BiometricPrompt (handled by Activity)
 * @param title Header text ("Unlock Gratia" or "Confirm Transaction")
 */
@Composable
fun LockScreen(
    lockMethod: SecurityManager.LockMethod,
    onPinVerified: () -> Unit,
    onPatternVerified: () -> Unit,
    onBiometricRequest: () -> Unit,
    title: String = "Unlock Gratia",
    modifier: Modifier = Modifier,
) {
    Box(
        modifier = modifier
            .fillMaxSize()
            .background(DeepNavy),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.padding(32.dp),
        ) {
            GratiaLogo(size = 72)

            Spacer(modifier = Modifier.height(16.dp))

            Text(
                text = title,
                style = MaterialTheme.typography.headlineMedium,
                fontWeight = FontWeight.Bold,
                color = WarmWhite,
            )

            Spacer(modifier = Modifier.height(32.dp))

            when (lockMethod) {
                SecurityManager.LockMethod.PIN -> {
                    PinEntry(
                        onPinComplete = { pin ->
                            if (SecurityManager.verifyPin(pin)) {
                                SecurityManager.onAuthSuccess()
                                onPinVerified()
                            }
                        },
                    )
                }
                SecurityManager.LockMethod.PATTERN -> {
                    PatternLock(
                        onPatternComplete = { pattern ->
                            if (SecurityManager.verifyPattern(pattern)) {
                                SecurityManager.onAuthSuccess()
                                onPatternVerified()
                            }
                        },
                    )
                }
                SecurityManager.LockMethod.BIOMETRIC,
                SecurityManager.LockMethod.DEVICE_CREDENTIAL -> {
                    BiometricPromptUI(onBiometricRequest = onBiometricRequest)
                }
                SecurityManager.LockMethod.NONE -> {
                    // Should not reach here
                }
            }
        }
    }
}

// ============================================================================
// 5-Digit PIN Entry
// ============================================================================

@Composable
fun PinEntry(
    onPinComplete: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    var pin by remember { mutableStateOf("") }
    var error by remember { mutableStateOf(false) }

    val errorColor by animateColorAsState(
        targetValue = if (error) Color.Red else Color.Transparent,
        animationSpec = tween(300),
        label = "pin_error",
    )

    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = modifier,
    ) {
        Text(
            text = "Enter 5-digit PIN",
            style = MaterialTheme.typography.bodyLarge,
            color = WarmWhite.copy(alpha = 0.7f),
        )

        Spacer(modifier = Modifier.height(16.dp))

        // PIN dots display
        Row(
            horizontalArrangement = Arrangement.spacedBy(16.dp),
            modifier = Modifier.padding(vertical = 8.dp),
        ) {
            repeat(5) { index ->
                Box(
                    modifier = Modifier
                        .size(20.dp)
                        .background(
                            color = when {
                                error -> Color.Red
                                index < pin.length -> AmberGold
                                else -> WarmWhite.copy(alpha = 0.3f)
                            },
                            shape = CircleShape,
                        ),
                )
            }
        }

        if (error) {
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                text = "Incorrect PIN",
                color = Color.Red,
                style = MaterialTheme.typography.bodySmall,
            )
        }

        Spacer(modifier = Modifier.height(24.dp))

        // Numeric keypad
        val keys = listOf(
            listOf("1", "2", "3"),
            listOf("4", "5", "6"),
            listOf("7", "8", "9"),
            listOf("", "0", "DEL"),
        )

        keys.forEach { row ->
            Row(
                horizontalArrangement = Arrangement.spacedBy(16.dp),
                modifier = Modifier.padding(vertical = 4.dp),
            ) {
                row.forEach { key ->
                    when (key) {
                        "" -> Spacer(modifier = Modifier.size(72.dp))
                        "DEL" -> {
                            IconButton(
                                onClick = {
                                    if (pin.isNotEmpty()) {
                                        pin = pin.dropLast(1)
                                        error = false
                                    }
                                },
                                modifier = Modifier.size(72.dp),
                            ) {
                                Icon(
                                    Icons.Default.Backspace,
                                    contentDescription = "Delete",
                                    tint = WarmWhite,
                                    modifier = Modifier.size(28.dp),
                                )
                            }
                        }
                        else -> {
                            Button(
                                onClick = {
                                    if (pin.length < 5) {
                                        pin += key
                                        error = false
                                        if (pin.length == 5) {
                                            val result = SecurityManager.verifyPin(pin)
                                            if (result) {
                                                GratiaHaptics.confirm(context)
                                                SecurityManager.onAuthSuccess()
                                                onPinComplete(pin)
                                            } else {
                                                GratiaHaptics.error(context)
                                                error = true
                                                pin = ""
                                            }
                                        }
                                    }
                                },
                                modifier = Modifier.size(72.dp),
                                shape = CircleShape,
                                colors = ButtonDefaults.buttonColors(
                                    containerColor = WarmWhite.copy(alpha = 0.1f),
                                ),
                            ) {
                                Text(
                                    text = key,
                                    fontSize = 24.sp,
                                    fontWeight = FontWeight.Bold,
                                    color = WarmWhite,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// Pattern Lock (3x3 grid)
// ============================================================================

@Composable
fun PatternLock(
    onPatternComplete: (List<Int>) -> Unit,
    modifier: Modifier = Modifier,
) {
    val patternContext = LocalContext.current
    val selectedDots = remember { mutableStateListOf<Int>() }
    var currentDrag by remember { mutableStateOf<Offset?>(null) }
    var error by remember { mutableStateOf(false) }

    // WHY: 280dp grid gives comfortable touch targets on screens 5" and up.
    // Each dot zone is ~93dp, with dots drawn at center of each zone.
    val gridSizeDp = 280.dp
    val density = LocalDensity.current
    val gridSizePx = with(density) { gridSizeDp.toPx() }
    val dotRadiusPx = with(density) { 14.dp.toPx() }
    val hitRadiusPx = with(density) { 40.dp.toPx() }

    fun dotCenter(index: Int): Offset {
        val row = index / 3
        val col = index % 3
        val cellSize = gridSizePx / 3
        return Offset(
            col * cellSize + cellSize / 2,
            row * cellSize + cellSize / 2,
        )
    }

    fun hitTest(offset: Offset): Int? {
        for (i in 0..8) {
            val center = dotCenter(i)
            val dist = (offset - center).getDistance()
            if (dist <= hitRadiusPx && i !in selectedDots) {
                return i
            }
        }
        return null
    }

    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = modifier,
    ) {
        Text(
            text = if (error) "Incorrect pattern — try again" else "Draw your pattern",
            style = MaterialTheme.typography.bodyLarge,
            color = if (error) Color.Red else WarmWhite.copy(alpha = 0.7f),
        )

        Spacer(modifier = Modifier.height(24.dp))

        Canvas(
            modifier = Modifier
                .size(gridSizeDp)
                .pointerInput(Unit) {
                    detectDragGestures(
                        onDragStart = { offset ->
                            selectedDots.clear()
                            error = false
                            hitTest(offset)?.let { selectedDots.add(it) }
                            currentDrag = offset
                        },
                        onDrag = { change, _ ->
                            currentDrag = change.position
                            hitTest(change.position)?.let { dot ->
                                if (dot !in selectedDots) {
                                    selectedDots.add(dot)
                                }
                            }
                        },
                        onDragEnd = {
                            currentDrag = null
                            if (selectedDots.size >= 4) {
                                val pattern = selectedDots.toList()
                                if (SecurityManager.verifyPattern(pattern)) {
                                    GratiaHaptics.confirm(patternContext)
                                    SecurityManager.onAuthSuccess()
                                    onPatternComplete(pattern)
                                } else {
                                    GratiaHaptics.error(patternContext)
                                    error = true
                                    selectedDots.clear()
                                }
                            } else {
                                error = true
                                selectedDots.clear()
                            }
                        },
                        onDragCancel = {
                            currentDrag = null
                            selectedDots.clear()
                        },
                    )
                },
        ) {
            val lineColor = if (error) Color.Red else AmberGold

            // Draw connecting lines between selected dots
            for (i in 0 until selectedDots.size - 1) {
                val from = dotCenter(selectedDots[i])
                val to = dotCenter(selectedDots[i + 1])
                drawLine(
                    color = lineColor,
                    start = from,
                    end = to,
                    strokeWidth = 6.dp.toPx(),
                    cap = StrokeCap.Round,
                )
            }

            // Draw line from last selected dot to current finger position
            if (selectedDots.isNotEmpty() && currentDrag != null) {
                val lastDot = dotCenter(selectedDots.last())
                drawLine(
                    color = lineColor.copy(alpha = 0.5f),
                    start = lastDot,
                    end = currentDrag!!,
                    strokeWidth = 4.dp.toPx(),
                    cap = StrokeCap.Round,
                )
            }

            // Draw dots
            for (i in 0..8) {
                val center = dotCenter(i)
                val isSelected = i in selectedDots
                val color = when {
                    error && isSelected -> Color.Red
                    isSelected -> AmberGold
                    else -> WarmWhite.copy(alpha = 0.4f)
                }
                // Outer ring
                drawCircle(
                    color = color,
                    radius = dotRadiusPx,
                    center = center,
                )
                // Inner dot (selected)
                if (isSelected) {
                    drawCircle(
                        color = color,
                        radius = dotRadiusPx * 0.5f,
                        center = center,
                    )
                }
            }
        }

        Spacer(modifier = Modifier.height(8.dp))

        Text(
            text = "Connect at least 4 dots",
            style = MaterialTheme.typography.bodySmall,
            color = WarmWhite.copy(alpha = 0.5f),
        )
    }
}

// ============================================================================
// Biometric / Device Credential prompt trigger
// ============================================================================

@Composable
fun BiometricPromptUI(
    onBiometricRequest: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = modifier,
    ) {
        Icon(
            Icons.Default.Fingerprint,
            contentDescription = "Biometric",
            tint = AmberGold,
            modifier = Modifier.size(80.dp),
        )

        Spacer(modifier = Modifier.height(16.dp))

        Text(
            text = "Touch the fingerprint sensor\nor use face unlock",
            style = MaterialTheme.typography.bodyLarge,
            color = WarmWhite.copy(alpha = 0.7f),
            textAlign = TextAlign.Center,
        )

        Spacer(modifier = Modifier.height(24.dp))

        Button(
            onClick = onBiometricRequest,
            colors = ButtonDefaults.buttonColors(
                containerColor = AmberGold,
            ),
            shape = RoundedCornerShape(12.dp),
        ) {
            Text(
                text = "Authenticate",
                fontWeight = FontWeight.Bold,
                color = DeepNavy,
            )
        }
    }
}

// ============================================================================
// PIN Setup (for Settings)
// ============================================================================

/**
 * PIN setup composable — used in Settings to create a new 5-digit PIN.
 * Requires entering the PIN twice for confirmation.
 */
@Composable
fun PinSetup(
    onPinSet: (String) -> Unit,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var step by remember { mutableStateOf(0) } // 0 = enter, 1 = confirm
    var firstPin by remember { mutableStateOf("") }
    var pin by remember { mutableStateOf("") }
    var error by remember { mutableStateOf(false) }

    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = modifier.padding(16.dp),
    ) {
        Text(
            text = if (step == 0) "Create a 5-digit PIN" else "Confirm your PIN",
            style = MaterialTheme.typography.titleLarge,
            fontWeight = FontWeight.Bold,
        )

        Spacer(modifier = Modifier.height(8.dp))

        // PIN dots
        Row(
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            modifier = Modifier.padding(vertical = 12.dp),
        ) {
            repeat(5) { index ->
                Box(
                    modifier = Modifier
                        .size(18.dp)
                        .background(
                            color = when {
                                error -> Color.Red
                                index < pin.length -> AmberGold
                                else -> MaterialTheme.colorScheme.outline.copy(alpha = 0.3f)
                            },
                            shape = CircleShape,
                        ),
                )
            }
        }

        if (error) {
            Text(
                text = "PINs don't match — try again",
                color = Color.Red,
                style = MaterialTheme.typography.bodySmall,
            )
            Spacer(modifier = Modifier.height(4.dp))
        }

        // Keypad
        val keys = listOf(
            listOf("1", "2", "3"),
            listOf("4", "5", "6"),
            listOf("7", "8", "9"),
            listOf("", "0", "DEL"),
        )

        keys.forEach { row ->
            Row(
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                modifier = Modifier.padding(vertical = 3.dp),
            ) {
                row.forEach { key ->
                    when (key) {
                        "" -> Spacer(modifier = Modifier.size(64.dp))
                        "DEL" -> {
                            IconButton(
                                onClick = {
                                    if (pin.isNotEmpty()) {
                                        pin = pin.dropLast(1)
                                        error = false
                                    }
                                },
                                modifier = Modifier.size(64.dp),
                            ) {
                                Icon(
                                    Icons.Default.Backspace,
                                    contentDescription = "Delete",
                                    modifier = Modifier.size(24.dp),
                                )
                            }
                        }
                        else -> {
                            Button(
                                onClick = {
                                    if (pin.length < 5) {
                                        pin += key
                                        error = false
                                        if (pin.length == 5) {
                                            if (step == 0) {
                                                firstPin = pin
                                                pin = ""
                                                step = 1
                                            } else {
                                                if (pin == firstPin) {
                                                    onPinSet(pin)
                                                } else {
                                                    error = true
                                                    pin = ""
                                                    step = 0
                                                    firstPin = ""
                                                }
                                            }
                                        }
                                    }
                                },
                                modifier = Modifier.size(64.dp),
                                shape = CircleShape,
                                colors = ButtonDefaults.buttonColors(
                                    containerColor = MaterialTheme.colorScheme.surfaceVariant,
                                ),
                            ) {
                                Text(
                                    text = key,
                                    fontSize = 20.sp,
                                    fontWeight = FontWeight.Bold,
                                )
                            }
                        }
                    }
                }
            }
        }

        Spacer(modifier = Modifier.height(12.dp))

        TextButton(onClick = onCancel) {
            Text("Cancel")
        }
    }
}

// ============================================================================
// Pattern Setup (for Settings)
// ============================================================================

/**
 * Pattern setup — draw twice to confirm.
 */
@Composable
fun PatternSetup(
    onPatternSet: (List<Int>) -> Unit,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var step by remember { mutableStateOf(0) } // 0 = draw, 1 = confirm
    var firstPattern by remember { mutableStateOf<List<Int>>(emptyList()) }
    var error by remember { mutableStateOf(false) }

    val selectedDots = remember { mutableStateListOf<Int>() }
    var currentDrag by remember { mutableStateOf<Offset?>(null) }

    val gridSizeDp = 240.dp
    val density = LocalDensity.current
    val gridSizePx = with(density) { gridSizeDp.toPx() }
    val dotRadiusPx = with(density) { 12.dp.toPx() }
    val hitRadiusPx = with(density) { 36.dp.toPx() }

    fun dotCenter(index: Int): Offset {
        val row = index / 3
        val col = index % 3
        val cellSize = gridSizePx / 3
        return Offset(col * cellSize + cellSize / 2, row * cellSize + cellSize / 2)
    }

    fun hitTest(offset: Offset): Int? {
        for (i in 0..8) {
            val center = dotCenter(i)
            if ((offset - center).getDistance() <= hitRadiusPx && i !in selectedDots) return i
        }
        return null
    }

    val outlineColor = MaterialTheme.colorScheme.outline
    val onSurfaceColor = MaterialTheme.colorScheme.onSurface

    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = modifier.padding(16.dp),
    ) {
        Text(
            text = when {
                error -> "Patterns don't match — try again"
                step == 0 -> "Draw a pattern (4+ dots)"
                else -> "Confirm your pattern"
            },
            style = MaterialTheme.typography.titleLarge,
            fontWeight = FontWeight.Bold,
            color = if (error) Color.Red else onSurfaceColor,
        )

        Spacer(modifier = Modifier.height(20.dp))

        Canvas(
            modifier = Modifier
                .size(gridSizeDp)
                .pointerInput(step, error) {
                    detectDragGestures(
                        onDragStart = { offset ->
                            selectedDots.clear()
                            error = false
                            hitTest(offset)?.let { selectedDots.add(it) }
                            currentDrag = offset
                        },
                        onDrag = { change, _ ->
                            currentDrag = change.position
                            hitTest(change.position)?.let { dot ->
                                if (dot !in selectedDots) selectedDots.add(dot)
                            }
                        },
                        onDragEnd = {
                            currentDrag = null
                            if (selectedDots.size >= 4) {
                                val pattern = selectedDots.toList()
                                if (step == 0) {
                                    firstPattern = pattern
                                    selectedDots.clear()
                                    step = 1
                                } else {
                                    if (pattern == firstPattern) {
                                        onPatternSet(pattern)
                                    } else {
                                        error = true
                                        selectedDots.clear()
                                        step = 0
                                        firstPattern = emptyList()
                                    }
                                }
                            } else {
                                selectedDots.clear()
                            }
                        },
                        onDragCancel = {
                            currentDrag = null
                            selectedDots.clear()
                        },
                    )
                },
        ) {
            val lineColor = if (error) Color.Red else AmberGold

            for (i in 0 until selectedDots.size - 1) {
                drawLine(lineColor, dotCenter(selectedDots[i]), dotCenter(selectedDots[i + 1]), 5.dp.toPx(), cap = StrokeCap.Round)
            }
            if (selectedDots.isNotEmpty() && currentDrag != null) {
                drawLine(lineColor.copy(alpha = 0.5f), dotCenter(selectedDots.last()), currentDrag!!, 3.dp.toPx(), cap = StrokeCap.Round)
            }
            for (i in 0..8) {
                val center = dotCenter(i)
                val selected = i in selectedDots
                val color = when {
                    error && selected -> Color.Red
                    selected -> AmberGold
                    else -> outlineColor.copy(alpha = 0.4f)
                }
                drawCircle(color, dotRadiusPx, center)
                if (selected) drawCircle(color, dotRadiusPx * 0.5f, center)
            }
        }

        Spacer(modifier = Modifier.height(16.dp))

        TextButton(onClick = onCancel) {
            Text("Cancel")
        }
    }
}
