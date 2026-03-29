package io.gratia.app.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.foundation.background
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
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AccountBalance
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.Fingerprint
import androidx.compose.material.icons.filled.HowToVote
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import io.gratia.app.ui.theme.AmberGold
import io.gratia.app.ui.theme.DeepNavy
import io.gratia.app.ui.theme.WarmWhite
import kotlinx.coroutines.launch

/**
 * Onboarding walkthrough shown on first launch.
 *
 * Four pages introduce the core Gratia concepts: earning crypto by using your
 * phone, Proof of Life verification, mining mechanics, and one-phone-one-vote
 * governance. Calls [onComplete] when the user taps "Get Started" or "Skip".
 */
@OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
@Composable
fun OnboardingScreen(onComplete: () -> Unit) {
    val pages = listOf(
        OnboardingPage(
            icon = Icons.Filled.AccountBalance,
            title = "Welcome to Gratia",
            description = "Your phone earns cryptocurrency just by being used.",
        ),
        OnboardingPage(
            icon = Icons.Filled.Fingerprint,
            title = "Proof of Life",
            description = "Use your phone normally. Gratia quietly verifies you\u2019re a real person \u2014 never tracking content, only patterns.",
        ),
        OnboardingPage(
            icon = Icons.Filled.Bolt,
            title = "Mine by Plugging In",
            description = "Plug in your phone with 80%+ battery and earn GRAT. Flat rate \u2014 every minute earns the same.",
        ),
        OnboardingPage(
            icon = Icons.Filled.HowToVote,
            title = "One Phone, One Vote",
            description = "No whales. No corporations. Every verified phone gets equal say in governance.",
        ),
    )

    val pagerState = rememberPagerState(pageCount = { pages.size })
    val coroutineScope = rememberCoroutineScope()
    val isLastPage = pagerState.currentPage == pages.size - 1

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(DeepNavy),
    ) {
        // Paged content
        HorizontalPager(
            state = pagerState,
            modifier = Modifier.fillMaxSize(),
        ) { pageIndex ->
            OnboardingPageContent(page = pages[pageIndex])
        }

        // Bottom controls: page dots + buttons
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .align(Alignment.BottomCenter)
                .padding(horizontal = 24.dp, vertical = 48.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            // Page indicator dots
            Row(
                horizontalArrangement = Arrangement.Center,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                repeat(pages.size) { index ->
                    PageDot(isSelected = index == pagerState.currentPage)
                    if (index < pages.size - 1) {
                        Spacer(modifier = Modifier.width(8.dp))
                    }
                }
            }

            Spacer(modifier = Modifier.height(32.dp))

            // Primary action button
            Button(
                onClick = {
                    if (isLastPage) {
                        onComplete()
                    } else {
                        coroutineScope.launch {
                            pagerState.animateScrollToPage(pagerState.currentPage + 1)
                        }
                    }
                },
                colors = ButtonDefaults.buttonColors(
                    containerColor = AmberGold,
                    contentColor = DeepNavy,
                ),
                modifier = Modifier
                    .fillMaxWidth()
                    .height(52.dp),
            ) {
                Text(
                    text = if (isLastPage) "Get Started" else "Next",
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                )
            }

            // Skip button — hidden on the last page
            if (!isLastPage) {
                TextButton(onClick = onComplete) {
                    Text(
                        text = "Skip",
                        color = WarmWhite.copy(alpha = 0.7f),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                }
            } else {
                // WHY: Reserve the same vertical space so the layout doesn't jump
                // when the user reaches the last page.
                Spacer(modifier = Modifier.height(48.dp))
            }
        }
    }
}

// ============================================================================
// Internal components
// ============================================================================

/**
 * Data class holding the content for a single onboarding page.
 */
private data class OnboardingPage(
    val icon: ImageVector,
    val title: String,
    val description: String,
)

/**
 * Full-screen content for one onboarding page: icon, title, and description.
 */
@Composable
private fun OnboardingPageContent(page: OnboardingPage) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(horizontal = 32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        // WHY: verticalArrangement = Center places the content in the visual
        // center of the screen, above the bottom control bar. The bottom bar
        // sits in a separate overlay, so centering here feels balanced.
        verticalArrangement = Arrangement.Center,
    ) {
        Icon(
            imageVector = page.icon,
            contentDescription = page.title,
            tint = AmberGold,
            modifier = Modifier.size(80.dp),
        )

        Spacer(modifier = Modifier.height(32.dp))

        Text(
            text = page.title,
            style = MaterialTheme.typography.headlineMedium,
            fontWeight = FontWeight.Bold,
            color = WarmWhite,
            textAlign = TextAlign.Center,
        )

        Spacer(modifier = Modifier.height(16.dp))

        Text(
            text = page.description,
            style = MaterialTheme.typography.bodyLarge,
            color = WarmWhite.copy(alpha = 0.7f),
            textAlign = TextAlign.Center,
        )
    }
}

/**
 * A single page indicator dot that animates between selected and unselected states.
 */
@Composable
private fun PageDot(isSelected: Boolean) {
    val size by animateDpAsState(
        targetValue = if (isSelected) 10.dp else 8.dp,
        label = "dotSize",
    )
    val color by animateColorAsState(
        targetValue = if (isSelected) AmberGold else WarmWhite.copy(alpha = 0.3f),
        label = "dotColor",
    )

    Box(
        modifier = Modifier
            .size(size)
            .clip(CircleShape)
            .background(color),
    )
}
