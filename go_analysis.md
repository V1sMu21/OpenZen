# Comprehensive Analysis of the Game of Go (围棋)

## 1. Full Rules of Go

### Basic Principles and Setup

Go is a two-player strategy board game originating in China over 2,500 years ago. The game is played on a grid of empty intersections where players alternately place black and white stones. The fundamental objective is to control more territory than the opponent by surrounding empty areas with one's stones.

**Board Dimensions:**
- Standard professional board: 19×19 grid (361 intersections)
- Smaller boards for practice: 13×13 or 9×9

**Equipment:**
- Two sets of stones: black (usually matte finish) and white (glossy finish)
- Go board traditionally made of wood, often Korean larch
- Scoring counters or markers

**Initial Setup:**
The board begins completely empty. Black plays first and places a stone on any vacant intersection.

### Basic Rules

**1. Placement Rule:**
Players alternately place one stone of their color on an empty intersection. Once placed, a stone remains on the board until removed through capture or at the end of play.

**2. Liberties and Capture:**
- A stone's liberties are the empty intersections immediately adjacent (horizontally or vertically) to it.
- When a stone or connected group of stones loses all its liberties, it is captured and removed from the board.
- Captured stones are placed in the opponent's prisoner pile.

**3. Turn Structure:**
- Players alternate turns, with Black moving first.
- A player may pass their turn if they believe no beneficial moves remain.
- The game ends when both players pass consecutively.

**4. Suicide Rule:**
A move that would leave one's own stones with no liberties is illegal, except when the move simultaneously captures opponent stones and creates liberties.

### Advanced Rules

#### Ko Rule

The ko (劫) rule prevents infinite repetition and promotes game progression.

**Basic Ko:**
When a stone is captured, the resulting board position repeats if the capturing stone can immediately be recaptured. The ko rule forbids immediate recapture, requiring the victim to make a move elsewhere first (ko threat) before retaking.

**Ko Sequence:**
```
Position 1: White captures one black stone
Position 2: Black cannot immediately recapture (ko rule)
Position 3: Black makes ko threat elsewhere
Position 4: White responds to threat or ignores
Position 5: Black retakes ko if appropriate
```

**Special Ko Variations:**
- **Triple Ko:** Three-fold repetition rule - game ends in draw or restart
- **Eternal Life (Chao):** Complex cycle that usually ends in draw

**Example Ko Diagram:**
```
    a b c d e
    5 . . . . .
    4 . X O X .
    3 . O ● O .
    2 . X O X .
    1 . . . . .

White captures black stone at c3. Black cannot immediately recapture at c3 due to ko rule.
```

#### Seki (Shared Life)

Seki occurs when two opposing groups share liberties, preventing either from playing to capture the other.

**Conditions for Seki:**
- Both groups have exactly one shared liberty
- Neither player can play on the shared liberty without losing stones
- Both groups are considered alive and safe

**Seki Example:**
```
    a b c d e
    5 . . . . .
    4 . X O X .
    3 . O ● O .
    2 . X O X .
    1 . . . . .

Stones at c3 and c2 share liberty at d3. Neither can play there without capture.
```

#### Komi (Compensation Points)

Komi compensates Black for playing first advantage.

**Standard Komi Values:**
- Japanese rules: 6.5 points (half-point to avoid draws)
- Chinese rules: 7.5 or 8.5 points depending on tournament
- AGA (American Go Association): 7.5 points

**Rationale:**
Statistical analysis shows Black wins approximately 52-54% of games without komi. Komi creates approximately equal winning chances.

#### Handicap Systems

Handicap stones allow players of different strengths to compete fairly.

**Handicap Placements:**
1-2 stones: Top three 4-4 points (star points)
3-5 stones: All four corners plus center
6-7 stones: All corners and edge points

**Handicap Rules:**
- Handicap stones are placed before play begins
- White plays first after handicap placement
- Each stone represents approximately one rank difference

### Scoring Methods

#### Japanese Rules (Japanese style)

**Territory Scoring:**
- Count surrounded empty intersections as territory
- Each captured stone adds 1 point to opponent's score
- Komi added to White's score

**Dead Stone Determination:**
- Players must agree on dead stones before counting
- Disagreements resolved by playing out the game

**Example Calculation:**
```
Black: Territory 30 + Captured 5 = 35 points
White: Territory 28 + Captured 6 + Komi 6.5 = 40.5 points
Result: White wins by 5.5 points
```

#### Chinese Rules (Chinese style)

**Area Scoring:**
- Count surrounded empty intersections
- Add number of one's own stones on board
- Each captured stone adds 1 point

**Formula:**
```
Total Score = Territory + Own Stones + Captured Stones + Komi
```

**Example:**
```
Black: Territory 30 + Stones 180 + Captured 5 = 215 points
White: Territory 28 + Stones 179 + Captured 6 + Komi 7.5 = 220.5 points
Result: White wins by 5.5 points
```

#### Area vs Territory Comparison

Both systems typically yield identical results except for:
- Prisoner counting differences
- Endgame play continuation preferences

### Game Termination

**Standard Termination:**
Game ends when both players pass consecutively. No further moves benefit either player.

**Agreed Termination:**
Players may agree to end game when further play serves no purpose.

**Time Limit Termination:**
Tournament games may end under time pressure with automatic termination.

### Tournament Rules Variations

**Byo-yomi:**
Time control system where players receive fixed time per move after main time expires.

**Japanese byo-yomi:**
1-minute periods, immediate loss on expiration.

**Finnish byo-yomi:**
Fixed time (e.g., 5 moves in 1 minute), bonus time per move completed.

**Canadian byo-yomi:**
Fixed group of moves (e.g., 20 moves in 1 hour).

**Seiko:**
Time forfeit rule - game ends when time expires regardless of position.

### Rule Disputes and Refereeing

**Professional Standards:**
Tournaments employ referees to settle disputes about:
- Dead stone identification
- Ko threats and sequences
- Time violations
- Board position recording

**Online Go Platforms:**
Computerized rule enforcement with automatic detection of:
- Illegal moves
- Ko violations
- Time management

### Summary

Go's elegant simplicity belies its strategic depth. The basic rules can be learned in minutes, yet mastery requires a lifetime of study. Understanding these fundamental rules provides the foundation for all advanced play and analysis.
